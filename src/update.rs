//! User-scoped GitHub Releases updates. No credentials in command lines or files.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const INSTALLER: &str = include_str!("../install.ps1");
static HELPER_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Deserialize)]
pub struct Release {
    pub version: String,
    pub tag: String,
}
#[derive(Clone, Deserialize)]
pub struct Prepared {
    pub version: String,
    pub directory: PathBuf,
}

#[derive(Clone)]
pub enum State {
    Idle,
    Checking,
    Available(Release),
    Current(String),
    Downloading,
    Ready(Prepared),
    Applying,
    Error(String),
}

enum Outcome {
    Checked(Release),
    Downloaded(Prepared),
}

pub struct Updater {
    pub state: State,
    receiver: Option<mpsc::Receiver<Result<Outcome, String>>>,
}

impl Default for Updater {
    fn default() -> Self {
        Self {
            state: State::Idle,
            receiver: None,
        }
    }
}

pub fn install_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("Programs/OhMyBackup")
}

pub fn cache_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("OhMyBackup/updates")
}

pub fn is_installed() -> bool {
    let manifest = std::fs::read(install_root().join("install.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    manifest.is_some_and(|v| v["product"] == "OhMyBackup")
        && std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .zip(install_root().join("oh-my-backup.exe").canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
}

pub fn version(value: &str) -> Result<[u32; 3]> {
    let parts = value
        .strip_prefix('v')
        .unwrap_or(value)
        .split('.')
        .collect::<Vec<_>>();
    if parts.len() != 3
        || parts.iter().any(|s| {
            s.is_empty()
                || (s.len() > 1 && s.starts_with('0'))
                || !s.bytes().all(|b| b.is_ascii_digit())
        })
    {
        bail!("Invalid stable version");
    }
    Ok([parts[0].parse()?, parts[1].parse()?, parts[2].parse()?])
}

fn powershell() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
        .join("System32/WindowsPowerShell/v1.0/powershell.exe")
}

fn helper_path() -> Result<PathBuf> {
    let root = cache_root().join("helpers");
    // Never write a trusted script through a redirected cache ancestor.
    for p in root.ancestors() {
        if let Ok(metadata) = std::fs::symlink_metadata(p) {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                bail!("Update cache cannot contain links or junctions");
            }
        }
    }
    std::fs::create_dir_all(&root)?;
    let path = root.join(format!(
        "installer-{}-{}.ps1",
        std::process::id(),
        HELPER_ID.fetch_add(1, Ordering::Relaxed)
            + std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_micros() as u64
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(&[0xef, 0xbb, 0xbf])?;
    file.write_all(INSTALLER.as_bytes())?;
    file.sync_all()?;
    Ok(path)
}

fn command(path: &Path) -> Command {
    let mut command = Command::new(powershell());
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(path)
        .creation_flags(0x0800_0000);
    command
}

#[derive(Serialize)]
struct Request<'a> {
    current_version: &'a str,
    tag: &'a str,
    token: &'a str,
}

fn run<T: serde::de::DeserializeOwned>(action: &str, request: Request<'_>) -> Result<T> {
    let path = helper_path()?;
    let mut child = command(&path)
        .args(["-Action", action])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Could not start update helper")?;
    child
        .stdin
        .take()
        .context("Update helper input unavailable")?
        .write_all(&serde_json::to_vec(&request)?)?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() > Duration::from_secs(300) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Update request timed out");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let output = child.wait_with_output()?;
    let _ = std::fs::remove_file(path);
    if !output.status.success() {
        // The helper emits only curated failures; never include a provided token.
        let mut error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if !request.token.is_empty() {
            error = error.replace(request.token, "[redacted]");
        }
        bail!(
            "{}",
            if error.is_empty() {
                "Update helper failed".into()
            } else {
                error
            }
        );
    }
    serde_json::from_slice(&output.stdout).context("Invalid update response")
}

pub fn check(token: &str) -> Result<Release> {
    let release: Release = run(
        "Check",
        Request {
            current_version: env!("CARGO_PKG_VERSION"),
            tag: "",
            token,
        },
    )?;
    if version(&release.tag)? != version(&release.version)? {
        bail!("Release tag/version mismatch");
    }
    Ok(release)
}

fn download(release: &Release, token: &str) -> Result<Prepared> {
    let prepared: Prepared = run(
        "Download",
        Request {
            current_version: env!("CARGO_PKG_VERSION"),
            tag: &release.tag,
            token,
        },
    )?;
    if prepared.version != release.version
        || prepared.directory.parent() != Some(cache_root().as_path())
    {
        bail!("Invalid prepared update response");
    }
    Ok(prepared)
}

impl Updater {
    pub fn busy(&self) -> bool {
        matches!(
            self.state,
            State::Checking | State::Downloading | State::Applying
        )
    }

    pub fn check(&mut self, token: String) {
        if self.busy() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.state = State::Checking;
        std::thread::spawn(move || {
            let _ = tx.send(
                check(&token)
                    .map(Outcome::Checked)
                    .map_err(|e| e.to_string()),
            );
        });
    }

    pub fn download(&mut self, release: Release, token: String) {
        if self.busy() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.state = State::Downloading;
        std::thread::spawn(move || {
            let _ = tx.send(
                download(&release, &token)
                    .map(Outcome::Downloaded)
                    .map_err(|e| e.to_string()),
            );
        });
    }

    pub fn poll(&mut self) {
        let Some(rx) = &self.receiver else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("Update worker stopped".into()),
        };
        self.receiver = None;
        self.state = match result {
            Ok(Outcome::Checked(release)) => match (
                version(&release.version),
                version(env!("CARGO_PKG_VERSION")),
            ) {
                (Ok(remote), Ok(local)) if remote > local => State::Available(release),
                (Ok(_), Ok(_)) => State::Current(release.version),
                _ => State::Error("Invalid release version".into()),
            },
            Ok(Outcome::Downloaded(package)) => State::Ready(package),
            Err(error) => State::Error(error),
        };
    }

    pub fn launch_apply(&mut self, prepared: &Prepared) -> Result<()> {
        if !is_installed() {
            bail!("Run install.ps1 before applying updates");
        }
        if prepared.directory.parent() != Some(cache_root().as_path()) {
            bail!("Invalid update cache path");
        }
        use windows_sys::Win32::{
            Foundation::FILETIME,
            System::Threading::{GetCurrentProcess, GetProcessTimes},
        };
        let (mut created, mut exited, mut kernel, mut user): (
            FILETIME,
            FILETIME,
            FILETIME,
            FILETIME,
        ) = unsafe { std::mem::zeroed() };
        if unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let created = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        let path = helper_path()?;
        command(&path)
            .args(["-Action", "Apply", "-PreparedDirectory"])
            .arg(&prepared.directory)
            .args([
                "-ParentId",
                &std::process::id().to_string(),
                "-ParentStartTicks",
                &created.to_string(),
                "-Restart",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Could not launch update installer")?;
        self.state = State::Applying;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_compare_numerically_and_reject_nonstable_inputs() {
        assert!(version("v0.10.0").unwrap() > version("0.2.0").unwrap());
        for invalid in [
            "",
            "v1.2",
            "1.2.3-beta",
            "01.2.3",
            "1.2.3.400",
            "1.2.-1",
            "1.2.999999999999999",
        ] {
            assert!(version(invalid).is_err(), "{invalid}");
        }
    }
    #[test]
    fn updater_accepts_completed_checks_and_worker_failures() {
        let mut updater = Updater::default();
        let (tx, rx) = mpsc::channel();
        updater.receiver = Some(rx);
        updater.state = State::Checking;
        tx.send(Ok(Outcome::Checked(Release {
            version: "999.0.0".into(),
            tag: "v999.0.0".into(),
        })))
        .unwrap();
        updater.poll();
        assert!(matches!(updater.state, State::Available(_)));
        let (tx, rx) = mpsc::channel();
        updater.receiver = Some(rx);
        updater.state = State::Downloading;
        drop(tx);
        updater.poll();
        assert!(matches!(updater.state, State::Error(_)));
    }

    #[test]
    fn saved_update_token_is_encrypted_and_can_be_removed() {
        let mut config = crate::config::Config::default();
        let token = "dummy-update-token-for-roundtrip";
        config.set_github_token(token).unwrap();
        assert!(!config.github_token_enc.contains(token));
        assert_eq!(config.github_token(), token);
        config.set_github_token("").unwrap();
        assert!(config.github_token_enc.is_empty());
        assert!(config.github_token().is_empty());
        assert!(config.set_github_token("bad\r\nheader").is_err());
        assert!(config.set_github_token(&"x".repeat(4097)).is_err());
    }
}
