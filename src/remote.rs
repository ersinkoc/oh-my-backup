//! SSH transport: runs the embedded Python helper on the server.

use crate::i18n;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use russh::ChannelMsg;
use russh::client;
use russh::keys::{HashAlg, PublicKeyOrCertificate};
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HELPER: &str = include_str!("helper.py");
pub const HASH_LEN: usize = 16;
pub type BlockHash = [u8; HASH_LEN];

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteFile {
    pub name: String,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteDir {
    pub name: String,
    pub busy: bool,
    pub files: Vec<RemoteFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteListing {
    pub dirs: Vec<RemoteDir>,
    pub disk_total: u64,
    pub disk_free: u64,
}

struct Handler {
    expected_fp: Option<String>,
    seen_fp: Arc<Mutex<Option<String>>>,
}

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let fp = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => {
                key.fingerprint(HashAlg::Sha256).to_string()
            }
            PublicKeyOrCertificate::Certificate(c) => {
                c.public_key().fingerprint(HashAlg::Sha256).to_string()
            }
        };
        *self.seen_fp.lock().unwrap() = Some(fp.clone());
        Ok(match &self.expected_fp {
            Some(e) if !e.is_empty() => *e == fp,
            _ => true,
        })
    }
}

pub struct Remote {
    handle: client::Handle<Handler>,
    pub fingerprint: String,
}

#[derive(Debug)]
pub struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(i18n::text("iptal edildi"))
    }
}
impl std::error::Error for Cancelled {}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn helper_cmd(args: &[&str]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(HELPER);
    let mut c = format!("python3 -c 'import base64;exec(base64.b64decode(\"{b64}\"))'");
    for a in args {
        c.push(' ');
        c.push_str(&sh_quote(a));
    }
    c
}

impl Remote {
    pub async fn connect(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
        expected_fp: Option<String>,
    ) -> Result<Remote> {
        let config = client::Config {
            inactivity_timeout: Some(Duration::from_secs(600)),
            keepalive_interval: Some(Duration::from_secs(20)),
            keepalive_max: 6,
            window_size: 16 * 1024 * 1024,
            ..Default::default()
        };
        let seen = Arc::new(Mutex::new(None));
        let h = Handler {
            expected_fp: expected_fp.clone(),
            seen_fp: seen.clone(),
        };
        let res = tokio::time::timeout(
            Duration::from_secs(20),
            client::connect(Arc::new(config), (host, port), h),
        )
        .await;
        let mut handle = match res {
            Err(_) => bail!(
                "{}",
                i18n::choose(
                    format!("bağlantı zaman aşımı ({host}:{port})"),
                    format!("connection timed out ({host}:{port})")
                )
            ),
            Ok(Err(e)) => {
                let seen = seen.lock().unwrap().clone();
                if let (Some(s), Some(exp)) = (seen, expected_fp)
                    && s != exp
                {
                    bail!(
                        "{}",
                        i18n::choose(
                            format!("SUNUCU ANAHTARI DEĞİŞTİ! beklenen {exp}, gelen {s}"),
                            format!("SERVER KEY CHANGED! Expected {exp}, received {s}")
                        )
                    );
                }
                return Err(anyhow!(e).context(i18n::text("SSH bağlantısı kurulamadı")));
            }
            Ok(Ok(h)) => h,
        };
        let auth = handle
            .authenticate_password(user, password)
            .await
            .context(i18n::text("kimlik doğrulama"))?;
        if !auth.success() {
            bail!(
                "{}",
                i18n::choose(
                    "kullanıcı adı/parola reddedildi".to_string(),
                    "username/password rejected".to_string()
                )
            );
        }
        let fingerprint = seen.lock().unwrap().clone().unwrap_or_default();
        Ok(Remote {
            handle,
            fingerprint,
        })
    }

    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// Run a helper command, streaming stdout chunks to `sink`. `cancel` is polled regularly.
    async fn exec_stream(
        &self,
        args: &[&str],
        stdin: Option<Vec<u8>>,
        mut sink: impl FnMut(&[u8]) -> Result<()> + Send,
        cancel: &(dyn Fn() -> bool + Sync),
    ) -> Result<()> {
        let mut ch = self
            .handle
            .channel_open_session()
            .await
            .context(i18n::text("kanal açılamadı"))?;
        ch.exec(true, helper_cmd(args)).await?;
        if let Some(data) = stdin {
            ch.data(&data[..]).await?;
        }
        ch.eof().await?;
        let mut stderr = Vec::new();
        let mut code = None;
        loop {
            if cancel() {
                let _ = ch.close().await;
                return Err(Cancelled.into());
            }
            let msg = match tokio::time::timeout(Duration::from_millis(500), ch.wait()).await {
                Err(_) => continue,
                Ok(None) => break,
                Ok(Some(m)) => m,
            };
            match msg {
                ChannelMsg::Data { data } => {
                    if let Err(e) = sink(&data) {
                        let _ = ch.close().await;
                        return Err(e);
                    }
                }
                ChannelMsg::ExtendedData { data, .. } => {
                    if stderr.len() < 8192 {
                        stderr.extend_from_slice(&data);
                    }
                }
                ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                _ => {}
            }
        }
        match code {
            Some(0) => Ok(()),
            c => bail!(
                "{}",
                i18n::choose(
                    format!(
                        "sunucu komutu başarısız (kod {:?}): {}",
                        c,
                        String::from_utf8_lossy(&stderr).trim()
                    ),
                    format!(
                        "server command failed (code {:?}): {}",
                        c,
                        String::from_utf8_lossy(&stderr).trim()
                    )
                )
            ),
        }
    }

    pub async fn list(&self, root: &str) -> Result<RemoteListing> {
        self.list_cancellable(root, &|| false).await
    }

    pub async fn list_cancellable(
        &self,
        root: &str,
        cancel: &(dyn Fn() -> bool + Sync),
    ) -> Result<RemoteListing> {
        let mut out = Vec::new();
        self.exec_stream(
            &["list", root],
            None,
            |d| {
                out.extend_from_slice(d);
                Ok(())
            },
            cancel,
        )
        .await?;
        serde_json::from_slice(&out).context(i18n::text("liste çözümlenemedi"))
    }

    pub async fn busy(&self, root: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        self.exec_stream(
            &["busy", root],
            None,
            |d| {
                out.extend_from_slice(d);
                Ok(())
            },
            &|| false,
        )
        .await?;
        Ok(serde_json::from_slice(&out)?)
    }

    /// Block hashes of a remote file starting at block `start`, streamed to `sink` as they arrive.
    pub async fn hash(
        &self,
        path: &str,
        block: u64,
        start: u64,
        mut sink: impl FnMut(&[BlockHash]) -> Result<()> + Send,
        cancel: &(dyn Fn() -> bool + Sync),
    ) -> Result<()> {
        let mut carry: Vec<u8> = Vec::new();
        let (bs, st) = (block.to_string(), start.to_string());
        self.exec_stream(
            &["hash", path, &bs, &st],
            None,
            |d| {
                carry.extend_from_slice(d);
                let whole = carry.len() / HASH_LEN * HASH_LEN;
                if whole > 0 {
                    let hs: Vec<BlockHash> = carry[..whole]
                        .chunks_exact(HASH_LEN)
                        .map(|c| c.try_into().unwrap())
                        .collect();
                    carry.drain(..whole);
                    sink(&hs)?;
                }
                Ok(())
            },
            cancel,
        )
        .await?;
        if !carry.is_empty() {
            bail!(
                "{}",
                i18n::choose(
                    "hash çıktısı bozuk".to_string(),
                    "corrupt hash output".to_string()
                )
            );
        }
        Ok(())
    }

    /// Stream the given byte ranges of a remote file, concatenated in order.
    pub async fn read_ranges(
        &self,
        path: &str,
        ranges: &[(u64, u64)],
        sink: impl FnMut(&[u8]) -> Result<()> + Send,
        cancel: &(dyn Fn() -> bool + Sync),
    ) -> Result<()> {
        let mut req = String::with_capacity(ranges.len() * 24);
        for (o, l) in ranges {
            req.push_str(&format!("{o} {l}\n"));
        }
        self.exec_stream(&["read", path], Some(req.into_bytes()), sink, cancel)
            .await
    }

    pub async fn close(self) {
        let _ = self
            .handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
    }
}

pub fn hash_block(data: &[u8]) -> BlockHash {
    let h = blake2b_simd::Params::new().hash_length(HASH_LEN).hash(data);
    h.as_bytes().try_into().unwrap()
}
