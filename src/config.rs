use crate::i18n;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateMode {
    /// Patch the existing .vbk in place (only changed blocks are written). Fast, gentle on the disk.
    InPlace,
    /// Build a full new copy next to the old set, swap when complete. Old set intact until the end.
    Safe,
}

/// How often a backup dir should be refreshed locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Plan {
    /// as soon as the server has a new restore point (outside job windows)
    Continuous,
    /// once every N calendar days
    EveryDays(u32),
    /// on the selected weekdays (bit 0 = Monday … bit 6 = Sunday)
    Weekdays(u8),
    /// Only when started manually.
    Manual,
}

pub const WEEKDAYS: [&str; 7] = ["Pzt", "Sal", "Çar", "Per", "Cum", "Cmt", "Paz"];

impl Plan {
    pub fn label(&self) -> String {
        match *self {
            Plan::Continuous => i18n::text("Sürekli (yeni yedek çıktıkça)").into(),
            Plan::EveryDays(1) => i18n::text("Her gün").into(),
            Plan::EveryDays(n) => i18n::choose(format!("{n} günde bir"), format!("Every {n} days")),
            Plan::Weekdays(m) => {
                let m = if m & 0x7f == 0 { 0x7f } else { m };
                if m & 0x7f == 0x7f {
                    i18n::text("Her gün (tüm günler)").into()
                } else {
                    WEEKDAYS
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| m & (1 << i) != 0)
                        .map(|(_, d)| i18n::text(d))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            }
            Plan::Manual => i18n::text("Sadece elle").into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub kind: crate::rules::ProfileKind,
    pub rules: crate::rules::FileRules,
    #[serde(skip)]
    pub storage_path: Option<PathBuf>,
    pub language: crate::i18n::Language,
    /// Optional GitHub token, DPAPI encrypted for this Windows user.
    pub github_token_enc: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// DPAPI-encrypted, base64
    pub password_enc: String,
    pub host_fingerprint: String,
    pub remote_root: String,
    pub local_root: String,
    pub margin_before_min: i64,
    pub margin_after_min: i64,
    pub hours_limit: bool,
    pub hours_start: u32,
    pub hours_end: u32,
    pub mode: UpdateMode,
    pub max_mbps: f64,
    pub auto_start: bool,
    pub scan_interval_min: u64,
    pub min_free_gb: u64,
    pub excluded: Vec<String>,
    /// remote dir name -> local legacy dir names approved for deletion after a successful sync
    pub legacy_approved: BTreeMap<String, Vec<String>>,
    /// delete approved legacy dirs *before* downloading when disk space is short
    pub free_space_first: bool,
    /// default refresh plan for all backup dirs
    pub plan: Plan,
    /// on a planned day, start no earlier than this hour (local)
    pub plan_hour: u32,
    /// per-dir plan overrides
    pub vm_plan: BTreeMap<String, Plan>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            kind: crate::rules::ProfileKind::Veeam,
            rules: crate::rules::FileRules::default(),
            storage_path: None,
            language: crate::i18n::Language::default(),
            github_token_enc: String::new(),
            host: String::new(),
            port: 22,
            user: "backup".into(),
            password_enc: String::new(),
            host_fingerprint: String::new(),
            remote_root: "/backup".into(),
            local_root: dirs::document_dir()
                .or_else(dirs::data_local_dir)
                .unwrap_or_else(std::env::temp_dir)
                .join("Oh My Backup")
                .to_string_lossy()
                .into(),
            margin_before_min: 30,
            margin_after_min: 20,
            hours_limit: false,
            hours_start: 8,
            hours_end: 19,
            mode: UpdateMode::InPlace,
            max_mbps: 0.0,
            auto_start: false,
            scan_interval_min: 10,
            min_free_gb: 20,
            excluded: Vec::new(),
            legacy_approved: BTreeMap::new(),
            free_space_first: true,
            plan: Plan::Continuous,
            plan_hour: 0,
            vm_plan: BTreeMap::new(),
        }
    }
}

pub fn app_dir() -> PathBuf {
    let d = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("HBSync");
    let _ = std::fs::create_dir_all(&d);
    d
}

impl Config {
    pub fn github_token(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(&self.github_token_enc)
            .ok()
            .and_then(|b| dpapi::unprotect(&b).ok())
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default()
    }

    pub fn set_github_token(&mut self, token: &str) -> Result<()> {
        use base64::Engine;
        if token.len() > 4096 || token.chars().any(char::is_whitespace) {
            anyhow::bail!("Invalid GitHub token: whitespace or excessive length");
        }
        self.github_token_enc = if token.is_empty() {
            String::new()
        } else {
            base64::engine::general_purpose::STANDARD.encode(dpapi::protect(token.as_bytes())?)
        };
        Ok(())
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.kind == crate::rules::ProfileKind::Files {
            self.rules.compile().map_err(|e| e.to_string())?;
            if self.mode != UpdateMode::Safe {
                return Err("File profiles require safe updates".into());
            }
        }
        if self.host.trim().is_empty() || self.user.trim().is_empty() || self.port == 0 {
            return Err(i18n::text("Sunucu, kullanıcı ve port (1–65535) gerekli").into());
        }
        if !self.remote_root.starts_with('/') {
            return Err(i18n::text("Sunucu yedek dizini '/' ile başlamalı").into());
        }
        if !Path::new(&self.local_root).is_absolute() {
            return Err(i18n::text("Yerel yedek dizini tam yol olmalı (örn. E:\\BACKUP)").into());
        }
        if !(1..=240).contains(&self.scan_interval_min) {
            return Err(i18n::text("Tarama aralığı 1–240 dakika olmalı").into());
        }
        if self.hours_start > 23 || self.hours_end > 23 || self.plan_hour > 23 {
            return Err(i18n::text("Saatler 0–23 arasında olmalı").into());
        }
        if !(0..=240).contains(&self.margin_before_min)
            || !(0..=240).contains(&self.margin_after_min)
        {
            return Err(i18n::text("Job bekleme payları 0–240 dakika olmalı").into());
        }
        if !self.max_mbps.is_finite()
            || !(0.0..=10000.0).contains(&self.max_mbps)
            || self.min_free_gb > 2000
        {
            return Err(i18n::text("Hız veya disk sınırı geçersiz").into());
        }
        Ok(())
    }
    pub fn path() -> PathBuf {
        match std::env::var_os("HBSYNC_CONFIG") {
            Some(p) => PathBuf::from(p),
            None => app_dir().join("config.json"),
        }
    }
    pub fn load() -> Config {
        Self::load_from(&Self::path())
    }
    pub fn load_from(p: &Path) -> Config {
        let Ok(b) = std::fs::read(p) else {
            return Config {
                storage_path: Some(p.to_owned()),
                ..Config::default()
            };
        };
        match serde_json::from_slice::<Config>(&b) {
            Ok(mut c) => {
                c.storage_path = Some(p.to_owned());
                c
            }
            Err(_) => {
                // keep the unreadable file instead of silently overwriting it later
                let _ = std::fs::copy(p, p.with_file_name("config.json.bad"));
                Config {
                    storage_path: Some(p.to_owned()),
                    ..Config::default()
                }
            }
        }
    }
    pub fn save(&self) -> Result<()> {
        write_atomic(&self.save_path(), &serde_json::to_vec_pretty(self)?)
    }
    pub fn save_path(&self) -> PathBuf {
        self.storage_path.clone().unwrap_or_else(Self::path)
    }
    pub fn password(&self) -> String {
        if self.password_enc.is_empty() {
            return String::new();
        }
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(&self.password_enc)
            .ok()
            .and_then(|b| dpapi::unprotect(&b).ok())
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default()
    }
    pub fn set_password(&mut self, pw: &str) {
        use base64::Engine;
        self.password_enc = dpapi::protect(pw.as_bytes())
            .map(|b| base64::engine::general_purpose::STANDARD.encode(b))
            .unwrap_or_default();
    }
    pub fn meta_dir(&self) -> PathBuf {
        meta_dir_for(&self.local_root)
    }
    pub fn plan_for(&self, name: &str) -> Plan {
        self.vm_plan.get(name).copied().unwrap_or(self.plan)
    }
}

pub fn meta_dir_for(local_root: &str) -> PathBuf {
    Path::new(local_root).join(".hbsync")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileRec {
    pub name: String,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct VmState {
    /// listing of the remote dir at the moment of the last successful commit
    pub committed: Option<Vec<FileRec>>,
    pub committed_at: Option<i64>,
    /// set while an in-place update has taken the .vbk out of the live set
    pub live_set_incomplete: bool,
    pub last_error: Option<String>,
    pub last_duration_s: Option<i64>,
    pub last_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct State {
    pub vms: BTreeMap<String, VmState>,
}

impl State {
    pub fn path(local_root: &str) -> PathBuf {
        meta_dir_for(local_root).join("state.json")
    }
    pub fn load(local_root: &str) -> State {
        std::fs::read(Self::path(local_root))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, local_root: &str) -> Result<()> {
        std::fs::create_dir_all(meta_dir_for(local_root))?;
        write_atomic(&Self::path(local_root), &serde_json::to_vec_pretty(self)?)
    }
}

/// Write to `<name>.tmp`, flush it to disk, then rename over the target. A power loss (USB disk!)
/// leaves either the old or the new content, never a half-written file.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut tmp_name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    {
        let mut f = std::fs::File::create(&tmp).with_context(|| {
            i18n::choose(
                format!("yazılamadı: {}", tmp.display()),
                format!("could not write: {}", tmp.display()),
            )
        })?;
        f.write_all(data).with_context(|| {
            i18n::choose(
                format!("yazılamadı: {}", tmp.display()),
                format!("could not write: {}", tmp.display()),
            )
        })?;
        f.sync_all().with_context(|| {
            i18n::choose(
                format!("diske yazılamadı: {}", tmp.display()),
                format!("could not flush to disk: {}", tmp.display()),
            )
        })?;
    }
    std::fs::rename(&tmp, path).with_context(|| {
        i18n::choose(
            format!("taşınamadı: {}", path.display()),
            format!("could not move: {}", path.display()),
        )
    })?;
    Ok(())
}

mod dpapi {
    use crate::i18n;
    use anyhow::{Result, bail};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
    };

    fn run(input: &[u8], protect: bool) -> Result<Vec<u8>> {
        unsafe {
            let inb = CRYPT_INTEGER_BLOB {
                cbData: input.len() as u32,
                pbData: input.as_ptr() as *mut u8,
            };
            let mut out = CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: std::ptr::null_mut(),
            };
            let ok = if protect {
                CryptProtectData(
                    &inb,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    &mut out,
                )
            } else {
                CryptUnprotectData(
                    &inb,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    &mut out,
                )
            };
            if ok == 0 {
                bail!(
                    "{}",
                    i18n::choose(
                        "DPAPI hatası".to_string(),
                        "DPAPI encryption error".to_string()
                    )
                );
            }
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(out.pbData as _);
            Ok(v)
        }
    }
    pub fn protect(d: &[u8]) -> Result<Vec<u8>> {
        run(d, true)
    }
    pub fn unprotect(d: &[u8]) -> Result<Vec<u8>> {
        run(d, false)
    }
}
