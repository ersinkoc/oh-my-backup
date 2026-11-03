//! Independent profile runtimes and destination ownership.
use crate::{
    config::{Config, UpdateMode, write_atomic},
    rules::{ProfileKind, normalized_root, overlaps},
    sync::Control,
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Default)]
pub struct RootLeases(Mutex<HashMap<String, String>>);
impl RootLeases {
    pub fn claim(&self, id: &str, root: &Path) -> Result<()> {
        let root = normalized_root(root)?;
        let mut leases = self.0.lock().unwrap();
        if leases
            .iter()
            .any(|(other, path)| other != id && overlaps(path, &root))
        {
            bail!("Destination overlaps another profile's active target");
        }
        leases.insert(id.into(), root);
        Ok(())
    }
    pub fn release(&self, id: &str) {
        self.0.lock().unwrap().remove(id);
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub kind: ProfileKind,
}
#[derive(Clone, Serialize, Deserialize)]
struct Catalog {
    schema: u32,
    selected: String,
    entries: Vec<Entry>,
}
pub struct Runtime {
    pub entry: Entry,
    pub ctl: Arc<Control>,
    pub started: bool,
}
pub struct Hub {
    pub profiles: Vec<Runtime>,
    pub selected: usize,
    path: Option<PathBuf>,
    base: PathBuf,
    leases: Arc<RootLeases>,
}

fn base_for(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".profiles");
    path.with_file_name(name)
}

impl Hub {
    pub fn single(ctl: Arc<Control>) -> Self {
        let kind = ctl.config.lock().unwrap().kind;
        Self {
            profiles: vec![Runtime {
                entry: Entry {
                    id: "legacy".into(),
                    name: if kind == ProfileKind::Veeam {
                        "Veeam"
                    } else {
                        "Files"
                    }
                    .into(),
                    kind,
                },
                ctl,
                started: false,
            }],
            selected: 0,
            path: None,
            base: PathBuf::new(),
            leases: Arc::default(),
        }
    }
    pub fn load() -> Result<Self> {
        Self::load_at(Config::path())
    }
    pub fn load_at(primary_path: PathBuf) -> Result<Self> {
        let primary = Config::load_from(&primary_path);
        let base = base_for(&primary_path);
        let path = base.join("catalog.json");
        let catalog: Catalog = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Catalog {
                schema: 1,
                selected: "legacy".into(),
                entries: vec![Entry {
                    id: "legacy".into(),
                    name: if primary.kind == ProfileKind::Veeam {
                        "Veeam"
                    } else {
                        "Files"
                    }
                    .into(),
                    kind: primary.kind,
                }],
            },
            Err(e) => return Err(e.into()),
        };
        if catalog.schema != 1
            || catalog.entries.is_empty()
            || catalog.entries[0].id != "legacy"
            || catalog.entries.len() > 32
        {
            bail!("Unsupported profile catalog");
        }
        let leases = Arc::new(RootLeases::default());
        let mut profiles = Vec::new();
        let mut ids = std::collections::HashSet::new();
        for entry in &catalog.entries {
            if entry.id.is_empty()
                || !entry
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                || !ids.insert(&entry.id)
                || entry.name.trim().is_empty()
            {
                bail!("Invalid profile identity");
            }
            let cfg = if entry.id == "legacy" {
                primary.clone()
            } else {
                let file = base.join(&entry.id).join("config.json");
                if !file.is_file() {
                    bail!("Profile settings are missing: {}", entry.name);
                }
                Config::load_from(&file)
            };
            if cfg.kind != entry.kind {
                bail!("Profile type mismatch: {}", entry.name);
            }
            let ctl = Control::new_scoped(cfg, entry.id.clone(), leases.clone());
            profiles.push(Runtime {
                entry: entry.clone(),
                ctl,
                started: false,
            });
        }
        let selected = profiles
            .iter()
            .position(|p| p.entry.id == catalog.selected)
            .unwrap_or(0);
        Ok(Self {
            profiles,
            selected,
            path: Some(path),
            base,
            leases,
        })
    }
    fn save_catalog(&self) -> Result<()> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Profile management is unavailable in preview mode"))?;
        crate::rules::plain_path(path)?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        let catalog = Catalog {
            schema: 1,
            selected: self.profiles[self.selected].entry.id.clone(),
            entries: self.profiles.iter().map(|p| p.entry.clone()).collect(),
        };
        write_atomic(path, &serde_json::to_vec_pretty(&catalog)?)
    }
    pub fn start(&mut self, index: usize) {
        if self.profiles[index].started {
            return;
        }
        self.profiles[index].started = true;
        let ctl = self.profiles[index].ctl.clone();
        crate::sync::start_sampler(ctl.clone());
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("profile runtime");
            runtime.block_on(crate::sync::engine(ctl));
        });
    }
    pub fn start_all(&mut self) {
        for i in 0..self.profiles.len() {
            self.start(i);
        }
    }
    pub fn select(&mut self, index: usize) -> Result<()> {
        if index >= self.profiles.len() {
            bail!("Profile not found");
        }
        let old = self.selected;
        self.selected = index;
        if let Err(e) = self.save_catalog() {
            self.selected = old;
            return Err(e);
        }
        Ok(())
    }
    pub fn validate_target(&self, index: usize, root: &str) -> Result<()> {
        let candidate = normalized_root(Path::new(root))?;
        for (other, runtime) in self.profiles.iter().enumerate() {
            if other == index {
                continue;
            }
            let cfg = runtime.ctl.config.lock().unwrap();
            if let Ok(existing) = normalized_root(Path::new(&cfg.local_root))
                && overlaps(&candidate, &existing)
            {
                bail!("Destination overlaps profile: {}", runtime.entry.name);
            }
        }
        Ok(())
    }
    pub fn add(&mut self, name: &str, kind: ProfileKind) -> Result<usize> {
        if self.path.is_none()
            || self.profiles.len() >= 32
            || name.trim().is_empty()
            || name.chars().count() > 64
        {
            bail!("Provide a profile name (maximum 32 profiles)");
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = format!(
            "p-{}-{}",
            chrono::Utc::now().timestamp_micros(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let file = self.base.join(&id).join("config.json");
        crate::rules::plain_path(&file)?;
        std::fs::create_dir_all(file.parent().unwrap())?;
        let primary = self.profiles[0].ctl.config.lock().unwrap().clone();
        let cfg = Config {
            kind,
            storage_path: Some(file),
            language: primary.language,
            mode: if kind == ProfileKind::Files {
                UpdateMode::Safe
            } else {
                UpdateMode::InPlace
            },
            scan_interval_min: if kind == ProfileKind::Files { 1 } else { 10 },
            local_root: Path::new(&Config::default().local_root)
                .join(&id)
                .to_string_lossy()
                .into(),
            ..Config::default()
        };
        cfg.save()?;
        let ctl = Control::new_scoped(cfg, id.clone(), self.leases.clone());
        let index = self.profiles.len();
        self.profiles.push(Runtime {
            entry: Entry {
                id,
                name: name.trim().into(),
                kind,
            },
            ctl,
            started: false,
        });
        if let Err(e) = self.save_catalog() {
            self.profiles.pop();
            return Err(e);
        }
        Ok(index)
    }
    pub fn rename(&mut self, index: usize, name: &str) -> Result<()> {
        if index >= self.profiles.len() || name.trim().is_empty() || name.chars().count() > 64 {
            bail!("Invalid profile name");
        }
        let old = self.profiles[index].entry.name.clone();
        self.profiles[index].entry.name = name.trim().into();
        if let Err(e) = self.save_catalog() {
            self.profiles[index].entry.name = old;
            return Err(e);
        }
        Ok(())
    }
    pub fn archive(&mut self, id: &str) -> Result<()> {
        let index = self
            .profiles
            .iter()
            .position(|p| p.entry.id == id)
            .ok_or_else(|| anyhow::anyhow!("Profile not found"))?;
        if index == 0 {
            bail!("The migrated primary profile cannot be archived");
        }
        let runtime = &self.profiles[index];
        if runtime.started
            && !(runtime.ctl.engine_stopped.load(Ordering::SeqCst)
                && runtime.ctl.sampler_stopped.load(Ordering::SeqCst))
        {
            bail!("Waiting for profile shutdown");
        }
        let old_selected = self.selected;
        let removed = self.profiles.remove(index);
        if self.selected == index {
            self.selected = 0;
        } else if self.selected > index {
            self.selected -= 1;
        }
        if let Err(e) = self.save_catalog() {
            self.profiles.insert(index, removed);
            self.selected = old_selected;
            return Err(e);
        }
        // Settings and all destination data are retained. Only the catalog entry is removed.
        Ok(())
    }
    pub fn request_shutdown(&self) {
        for profile in &self.profiles {
            profile.ctl.running.store(false, Ordering::SeqCst);
            profile.ctl.cancel.store(true, Ordering::SeqCst);
            profile.ctl.shutdown.store(true, Ordering::SeqCst);
            profile.ctl.wake();
        }
    }
    pub fn stopped(&self) -> bool {
        self.profiles.iter().all(|p| {
            !p.started
                || (p.ctl.engine_stopped.load(Ordering::SeqCst)
                    && p.ctl.sampler_stopped.load(Ordering::SeqCst))
        })
    }
    pub fn busy(&self) -> bool {
        self.profiles
            .iter()
            .any(|p| p.ctl.shared.lock().unwrap().activity.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_migration_preserves_settings_and_profile_saves_are_isolated() {
        let root = std::env::temp_dir().join(format!("omb-profiles-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        std::fs::write(
            &path,
            br#"{"host":"backup.example.invalid","local_root":"E:\\ARCHIVE","auto_start":false}"#,
        )
        .unwrap();
        let mut hub = Hub::load_at(path.clone()).unwrap();
        assert_eq!(
            hub.profiles[0].ctl.config.lock().unwrap().host,
            "backup.example.invalid"
        );
        assert_eq!(hub.profiles[0].entry.kind, ProfileKind::Veeam);
        let added = hub.add("Documents", ProfileKind::Files).unwrap();
        hub.select(added).unwrap();
        {
            let mut cfg = hub.profiles[added].ctl.config.lock().unwrap();
            cfg.host = "files.example.invalid".into();
            cfg.save().unwrap();
        }
        assert_eq!(Config::load_from(&path).host, "backup.example.invalid");
        let restored = Hub::load_at(path).unwrap();
        assert_eq!(restored.selected, added);
        assert_eq!(
            restored.profiles[added].ctl.config.lock().unwrap().host,
            "files.example.invalid"
        );
        assert!(!restored.profiles[added].ctl.running.load(Ordering::SeqCst));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn target_leases_reject_nested_targets_but_not_similar_siblings() {
        let root = std::env::temp_dir().join("omb-root-lease");
        let leases = RootLeases::default();
        leases.claim("a", &root).unwrap();
        assert!(leases.claim("b", &root.join("nested")).is_err());
        assert!(
            leases
                .claim("b", &root.with_file_name("omb-root-lease-other"))
                .is_ok()
        );
        leases.release("a");
        assert!(leases.claim("c", &root.join("nested")).is_ok());
    }
    #[test]
    fn archiving_retains_settings_and_destination_data() {
        let root = std::env::temp_dir().join(format!("omb-profile-archive-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut hub = Hub::load_at(root.join("config.json")).unwrap();
        let index = hub.add("Archive", ProfileKind::Files).unwrap();
        let settings = hub.profiles[index].ctl.config.lock().unwrap().save_path();
        let destination = root.join("destination");
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(destination.join("owned.txt"), b"keep").unwrap();
        {
            let mut cfg = hub.profiles[index].ctl.config.lock().unwrap();
            cfg.local_root = destination.to_string_lossy().into();
            cfg.save().unwrap();
        }
        let id = hub.profiles[index].entry.id.clone();
        hub.select(index).unwrap();
        assert!(hub.archive("legacy").is_err());
        hub.archive(&id).unwrap();
        assert!(settings.exists());
        assert_eq!(
            std::fs::read(destination.join("owned.txt")).unwrap(),
            b"keep"
        );
        assert_eq!(hub.selected, 0);
        let restored = Hub::load_at(root.join("config.json")).unwrap();
        assert_eq!(restored.profiles.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
