//! Recursive, rule-driven pull sync. Planning is pure; only managed files may be deleted.
use crate::{
    config::{Config, Plan, write_atomic},
    i18n,
    remote::{Remote, RemoteFile},
    rules::{Deletion, plain_path, target, validate_paths},
    schedule,
    sync::{Activity, Control, LocalFile, VmView},
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFile {
    pub name: String,
    pub size: u64,
    pub mtime: i64,
    pub mtime_ns: i64,
}
impl SourceFile {
    fn remote(&self) -> RemoteFile {
        RemoteFile {
            name: self.name.clone(),
            size: self.size,
            mtime: self.mtime,
        }
    }
    fn transfer_version(&self) -> RemoteFile {
        RemoteFile {
            name: self.name.clone(),
            size: self.size,
            mtime: self.mtime_ns,
        }
    }
}
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Snapshot {
    pub files: Vec<SourceFile>,
    pub disk_total: u64,
    pub disk_free: u64,
    #[serde(default)]
    pub context: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalStamp {
    pub size: u64,
    pub modified_ns: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub source: SourceFile,
    pub local: LocalStamp,
    pub at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    file: SourceFile,
    since: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub at: i64,
    pub copied: usize,
    pub removed: usize,
    pub bytes: u64,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FileState {
    pub records: BTreeMap<String, Record>,
    observations: BTreeMap<String, Observation>,
    pub history: Vec<Run>,
    source: Option<String>,
    missing_since: BTreeMap<String, i64>,
    errors: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Copy,
    Delete,
    Quarantine,
    Retain,
    Forget,
    Wait,
    Excluded,
    Conflict,
    Current,
}
#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub action: Action,
    pub reason: String,
    pub bytes: u64,
}
impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Copy => i18n::choose("Aktar", "Transfer"),
            Self::Delete => i18n::choose("Sil", "Delete"),
            Self::Quarantine => i18n::choose("Karantina", "Quarantine"),
            Self::Retain => i18n::choose("Yerelde sakla", "Retain locally"),
            Self::Forget => i18n::choose("Kaydı kapat", "Forget missing copy"),
            Self::Wait => i18n::choose("Bekle", "Wait"),
            Self::Excluded => i18n::choose("Hariç", "Excluded"),
            Self::Conflict => i18n::choose("Çakışma", "Conflict"),
            Self::Current => i18n::choose("Güncel", "Current"),
        }
    }
}

fn meta(root: &str) -> PathBuf {
    crate::config::meta_dir_for(root).join("generic")
}
pub(crate) fn cache_dir(cfg: &Config) -> PathBuf {
    let digest = blake2b_simd::Params::new()
        .hash_length(16)
        .hash(source_context(cfg).as_bytes())
        .to_hex()
        .to_string();
    meta(&cfg.local_root).join("sources").join(digest)
}
fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        chrono::Utc::now().timestamp_micros(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Serialize, Deserialize)]
struct Journal {
    name: String,
    record: Option<Record>,
}

impl FileState {
    pub fn check_source(&self, source: &str) -> Result<()> {
        if self.source.as_deref() != Some(source) && !self.records.is_empty() {
            bail!("Destination belongs to a different source. Choose a separate target folder.");
        }
        Ok(())
    }
    pub fn load(root: &str) -> Result<Self> {
        let directory = meta(root);
        plain_path(&directory)?;
        let path = directory.join("state.json");
        plain_path(&path)?;
        let mut state: Self = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e.into()),
        };
        let journal = directory.join("journal.jsonl");
        plain_path(&journal)?;
        match fs::read(&journal) {
            Ok(bytes) => {
                // A torn final append has no durable ownership record; never infer one.
                let complete = bytes
                    .iter()
                    .rposition(|b| *b == b'\n')
                    .map(|p| &bytes[..=p])
                    .unwrap_or(&[]);
                for line in complete.split(|b| *b == b'\n').filter(|s| !s.is_empty()) {
                    let entry: Journal = serde_json::from_slice(line)?;
                    crate::rules::validate_relative(&entry.name)?;
                    if let Some(record) = entry.record {
                        if record.source.name != entry.name {
                            bail!("Invalid ownership journal");
                        }
                        state.records.insert(entry.name, record);
                    } else {
                        state.records.remove(&entry.name);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        validate_paths(state.records.keys().map(String::as_str))?;
        if state
            .records
            .iter()
            .any(|(name, record)| *name != record.source.name)
        {
            bail!("Invalid managed-file identity");
        }
        Ok(state)
    }
    pub fn save(&self, root: &str) -> Result<()> {
        let directory = meta(root);
        plain_path(&directory)?;
        fs::create_dir_all(&directory)?;
        let state = directory.join("state.json");
        plain_path(&state)?;
        write_atomic(&state, &serde_json::to_vec(self)?)?;
        let journal = directory.join("journal.jsonl");
        plain_path(&journal)?;
        // Clearing after the durable checkpoint is safe: replay is idempotent.
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(journal)?;
        file.sync_all()?;
        Ok(())
    }
    fn record(&mut self, root: &str, name: &str, record: Option<Record>) -> Result<()> {
        let directory = meta(root);
        plain_path(&directory)?;
        fs::create_dir_all(&directory)?;
        let journal = directory.join("journal.jsonl");
        plain_path(&journal)?;
        let mut line = serde_json::to_vec(&Journal {
            name: name.into(),
            record: record.clone(),
        })?;
        line.push(b'\n');
        let mut file = OpenOptions::new().create(true).append(true).open(journal)?;
        file.write_all(&line)?;
        file.sync_all()?;
        if let Some(record) = record {
            self.records.insert(name.into(), record);
        } else {
            self.records.remove(name);
        }
        Ok(())
    }
    pub fn observe(&mut self, snapshot: &Snapshot, now: i64) {
        let names = snapshot
            .files
            .iter()
            .map(|f| f.name.as_str())
            .collect::<std::collections::HashSet<_>>();
        self.observations
            .retain(|name, _| names.contains(name.as_str()));
        self.missing_since
            .retain(|name, _| self.records.contains_key(name) && !names.contains(name.as_str()));
        for name in self
            .records
            .keys()
            .filter(|name| !names.contains(name.as_str()))
        {
            let since = self.missing_since.entry(name.clone()).or_insert(now);
            if *since > now {
                *since = now;
            }
        }
        for file in &snapshot.files {
            let observation = self
                .observations
                .entry(file.name.clone())
                .or_insert_with(|| Observation {
                    file: file.clone(),
                    since: now,
                });
            if observation.file != *file || observation.since > now {
                *observation = Observation {
                    file: file.clone(),
                    since: now,
                };
            }
        }
    }
}

pub fn local_stamp(path: &Path) -> Result<Option<LocalStamp>> {
    plain_path(path)?;
    match fs::metadata(path) {
        Ok(m) if m.is_file() => Ok(Some(LocalStamp {
            size: m.len(),
            modified_ns: m
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .min(u64::MAX as u128) as u64,
        })),
        Ok(_) => bail!("Destination file conflicts with a directory"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn local_files(
    cfg: &Config,
    snapshot: &Snapshot,
    state: &FileState,
) -> Result<BTreeMap<String, LocalStamp>> {
    let mut names = snapshot
        .files
        .iter()
        .map(|f| f.name.clone())
        .chain(state.records.keys().cloned())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    let mut local = BTreeMap::new();
    for name in names {
        if let Some(stamp) = local_stamp(&target(Path::new(&cfg.local_root), &name)?)? {
            local.insert(name, stamp);
        }
    }
    Ok(local)
}

pub fn plan(
    cfg: &Config,
    snapshot: &Snapshot,
    state: &FileState,
    local: &BTreeMap<String, LocalStamp>,
    now: i64,
    forced: Option<&str>,
    running: bool,
) -> Result<Vec<Item>> {
    if snapshot.files.len() > 100_000 {
        bail!("Source exceeds 100,000 files");
    }
    validate_paths(snapshot.files.iter().map(|f| f.name.as_str()))?;
    let matcher = cfg.rules.compile()?;
    let mut result = Vec::new();
    for file in &snapshot.files {
        let old = state.records.get(&file.name);
        let actual = local.get(&file.name);
        let manual = forced == Some(file.name.as_str());
        let excluded = !matcher.matches(&file.name) || cfg.excluded.contains(&file.name);
        let (action, reason) = if excluded {
            (Action::Excluded, "Excluded by source rules")
        } else if actual.is_some_and(|l| old.is_some_and(|r| r.source == *file && r.local == *l)) {
            (Action::Current, "Source and managed local copy match")
        } else if actual.is_some() && old.is_none() && !cfg.rules.overwrite_unmanaged {
            (
                Action::Conflict,
                "Existing local file has no ownership record",
            )
        } else if cfg.rules.protect_local_changes
            && actual.is_some_and(|l| old.is_some_and(|r| r.local != *l))
        {
            (
                Action::Conflict,
                "Local file changed outside the application",
            )
        } else if cfg.rules.quiet_seconds > 0
            && state.observations.get(&file.name).is_none_or(|o| {
                o.file != *file || now.saturating_sub(o.since) < cfg.rules.quiet_seconds as i64
            })
        {
            (Action::Wait, "Waiting for the quiet period")
        } else if !running && !manual {
            (Action::Wait, "Automatic sync is paused")
        } else if !manual
            && actual.is_some()
            && schedule::plan_due_at(cfg.plan_for(&file.name), cfg.plan_hour, old.map(|r| r.at))
                .is_none_or(|due| due > now)
        {
            (Action::Wait, "Waiting for the schedule")
        } else if !manual && cfg.plan_for(&file.name) == Plan::Manual {
            (Action::Wait, "Manual-only plan")
        } else if !manual
            && cfg.hours_limit
            && !schedule::hours_allowed(now, cfg.hours_start, cfg.hours_end)
        {
            (Action::Wait, "Outside allowed hours")
        } else {
            (
                Action::Copy,
                if old.is_none() {
                    "New source file"
                } else if actual.is_none() {
                    "Repair missing local copy"
                } else {
                    "Source file changed"
                },
            )
        };
        result.push(Item {
            name: file.name.clone(),
            action,
            reason: reason.into(),
            bytes: if action == Action::Copy { file.size } else { 0 },
        });
    }
    let remote_names = snapshot
        .files
        .iter()
        .map(|f| f.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    for name in state
        .records
        .keys()
        .filter(|name| !remote_names.contains(name.as_str()))
    {
        let (action, reason) = if !matcher.matches(name) || cfg.excluded.contains(name) {
            (Action::Excluded, "Excluded by current rules")
        } else if !local.contains_key(name) {
            (Action::Forget, "Neither source nor local copy exists")
        } else if cfg.rules.deletion == Deletion::Keep {
            (Action::Retain, "Source removed; local copy retained")
        } else if cfg.rules.protect_local_changes
            && local.get(name) != state.records.get(name).map(|r| &r.local)
        {
            (
                Action::Conflict,
                "Local changes prevent source-deletion propagation",
            )
        } else if !running && forced != Some(name.as_str()) {
            (Action::Wait, "Automatic sync is paused")
        } else if cfg.rules.quiet_seconds > 0
            && state
                .missing_since
                .get(name)
                .is_none_or(|since| now.saturating_sub(*since) < cfg.rules.quiet_seconds as i64)
        {
            (Action::Wait, "Waiting for source deletion to settle")
        } else if forced != Some(name.as_str())
            && schedule::plan_due_at(
                cfg.plan_for(name),
                cfg.plan_hour,
                state.records.get(name).map(|r| r.at),
            )
            .is_none_or(|due| due > now)
        {
            (Action::Wait, "Waiting for the schedule")
        } else if forced != Some(name.as_str())
            && cfg.hours_limit
            && !schedule::hours_allowed(now, cfg.hours_start, cfg.hours_end)
        {
            (Action::Wait, "Outside allowed hours")
        } else if cfg.rules.deletion == Deletion::Quarantine {
            (
                Action::Quarantine,
                "Move managed local copy into quarantine",
            )
        } else {
            (
                Action::Delete,
                "Mirror source deletion for this managed file",
            )
        };
        result.push(Item {
            name: name.clone(),
            action,
            reason: reason.into(),
            bytes: 0,
        });
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

fn install_file(cfg: &Config, name: &str, staged: &Path) -> Result<LocalStamp> {
    let destination = target(Path::new(&cfg.local_root), name)?;
    plain_path(staged)?;
    fs::create_dir_all(destination.parent().unwrap())?;
    if destination.exists() {
        if !destination.is_file() {
            bail!("Destination conflicts with a directory");
        }
        let versions = target(&meta(&cfg.local_root).join("versions"), name)?;
        fs::create_dir_all(&versions)?;
        let previous = versions.join(format!("{}.bak", unique()));
        plain_path(&previous)?;
        use std::os::windows::ffi::OsStrExt;
        let wide = |p: &Path| {
            p.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        let (to, from, backup) = (wide(&destination), wide(staged), wide(&previous));
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::ReplaceFileW(
                to.as_ptr(),
                from.as_ptr(),
                backup.as_ptr(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let pattern = regex::Regex::new(r"^[0-9]+-[0-9]+-[0-9]+\.bak$").unwrap();
        let mut copies = fs::read_dir(&versions)?
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .filter(|e| {
                e.file_type().is_ok_and(|t| t.is_file())
                    && pattern.is_match(&e.file_name().to_string_lossy())
            })
            .map(|e| e.path())
            .collect::<Vec<_>>();
        copies.sort();
        let excess = copies.len().saturating_sub(cfg.rules.keep_versions);
        for copy in copies.into_iter().take(excess) {
            plain_path(&copy)?;
            fs::remove_file(copy)?;
        }
    } else {
        // Fail rather than overwrite a file that appeared after planning.
        use std::os::windows::ffi::OsStrExt;
        let from = staged
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let to = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    local_stamp(&destination)?.ok_or_else(|| anyhow::anyhow!("Installed file disappeared"))
}

fn remove_file(cfg: &Config, state: &mut FileState, name: &str, action: Action) -> Result<()> {
    let expected = state
        .records
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("Cannot remove an unmanaged file"))?;
    let path = target(Path::new(&cfg.local_root), name)?;
    if action != Action::Forget
        && cfg.rules.protect_local_changes
        && local_stamp(&path)?.as_ref() != Some(&expected.local)
    {
        bail!("Local copy changed before deletion");
    }
    match action {
        Action::Quarantine => {
            let directory = meta(&cfg.local_root).join("quarantine").join(unique());
            let to = target(&directory, name)?;
            fs::create_dir_all(to.parent().unwrap())?;
            fs::rename(path, to)?;
        }
        Action::Delete => fs::remove_file(path)?,
        Action::Forget => {}
        _ => bail!("Not a removal action"),
    }
    state.record(&cfg.local_root, name, None)
}

fn views(
    ctl: &Control,
    cfg: &Config,
    snapshot: &Snapshot,
    state: &FileState,
    local: &BTreeMap<String, LocalStamp>,
    items: &[Item],
) {
    let sources = snapshot
        .files
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect::<BTreeMap<_, _>>();
    let mut shared = ctl.shared.lock().unwrap();
    shared.vms = items
        .iter()
        .map(|item| {
            let source = sources.get(item.name.as_str()).copied();
            let old = state.records.get(&item.name);
            let stamp = local.get(&item.name);
            VmView {
                file_action: Some(item.action),
                file_reason: Some(item.reason.clone()),
                name: item.name.clone(),
                remote: source.map(|f| vec![f.remote()]).unwrap_or_default(),
                remote_size: source.map(|f| f.size).unwrap_or(0),
                busy: false,
                job: None,
                local: stamp
                    .map(|s| {
                        vec![LocalFile {
                            name: item.name.clone(),
                            size: s.size,
                        }]
                    })
                    .unwrap_or_default(),
                local_size: stamp.map(|s| s.size).unwrap_or(0),
                local_exists: stamp.is_some(),
                committed_at: old.map(|r| r.at),
                committed: old
                    .map(|r| {
                        vec![crate::config::FileRec {
                            name: r.source.name.clone(),
                            size: r.source.size,
                            mtime: r.source.mtime,
                        }]
                    })
                    .unwrap_or_default(),
                in_sync: item.action == Action::Current,
                incomplete: false,
                excluded: item.action == Action::Excluded,
                legacy: vec![],
                staging_files: vec![],
                status: format!("{} · {}", item.action.label(), item.reason),
                last_error: state
                    .errors
                    .get(&item.name)
                    .cloned()
                    .or_else(|| (item.action == Action::Conflict).then(|| item.reason.clone())),
                est_bytes: item.bytes,
                plan: cfg.plan_for(&item.name),
                plan_custom: cfg.vm_plan.contains_key(&item.name),
                due_at: schedule::plan_due_at(
                    cfg.plan_for(&item.name),
                    cfg.plan_hour,
                    old.map(|r| r.at),
                ),
                retry_at: ctl.retry.lock().unwrap().get(&item.name).map(|r| r.0),
                snoozed: ctl.snoozed.lock().unwrap().contains(&item.name),
            }
        })
        .collect();
    shared.last_scan = Some(chrono::Utc::now().timestamp());
    shared.remote_disk = Some((snapshot.disk_total, snapshot.disk_free));
    shared.local_free = fs2::available_space(&cfg.local_root).unwrap_or(0);
    shared.local_total = fs2::total_space(&cfg.local_root).unwrap_or(0);
}

async fn wait(ctl: &Control, seconds: u64) {
    tokio::select! { _ = tokio::time::sleep(Duration::from_secs(seconds)) => {}, _ = ctl.notify.notified() => {} }
}

pub fn source_context(cfg: &Config) -> String {
    serde_json::to_string(&(
        &cfg.host,
        cfg.port,
        &cfg.user,
        &cfg.host_fingerprint,
        &cfg.remote_root,
    ))
    .unwrap()
}

pub async fn engine(ctl: &Arc<Control>) {
    let mut remote: Option<Arc<Remote>> = None;
    loop {
        if ctl.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let cfg = ctl.config.lock().unwrap().clone();
        let outcome = pass(ctl, &cfg, &mut remote).await;
        ctl.set_activity(None);
        if let Err(error) = outcome {
            ctl.shared.lock().unwrap().waiting = error.to_string();
            ctl.log(format!("✖ {}", error));
        }
        ctl.repaint();
        if ctl.once.load(Ordering::SeqCst) {
            return;
        }
        wait(ctl, cfg.scan_interval_min.max(1) * 60).await;
    }
}

async fn pass(ctl: &Arc<Control>, cfg: &Config, remote: &mut Option<Arc<Remote>>) -> Result<()> {
    cfg.validate().map_err(anyhow::Error::msg)?;
    ctl.claim_destination(cfg)?;
    let root = Path::new(&cfg.local_root);
    plain_path(root)?;
    for protected in [
        cfg.save_path()
            .parent()
            .unwrap_or(Path::new("."))
            .to_owned(),
        crate::update::install_root(),
    ] {
        if crate::rules::overlaps(
            &crate::rules::normalized_root(root)?,
            &crate::rules::normalized_root(&protected)?,
        ) {
            bail!("Destination overlaps application settings or installation");
        }
    }
    fs::create_dir_all(root)?;
    if ctl.reconnect.swap(false, Ordering::SeqCst) || remote.as_ref().is_some_and(|r| r.is_closed())
    {
        *remote = None;
    }
    if remote.is_none() {
        {
            let mut shared = ctl.shared.lock().unwrap();
            shared.conn = i18n::choose("Bağlanıyor…", "Connecting…").into();
            shared.connected = false;
        }
        ctl.repaint();
        let password = cfg.password();
        if password.is_empty() {
            bail!("Configure the profile connection and password in Settings");
        }
        let connected = Remote::connect(
            &cfg.host,
            cfg.port,
            &cfg.user,
            &password,
            (!cfg.host_fingerprint.is_empty()).then(|| cfg.host_fingerprint.clone()),
        )
        .await?;
        if cfg.host_fingerprint.is_empty() {
            let mut current = ctl.config.lock().unwrap();
            if serde_json::to_vec(&*current)? != serde_json::to_vec(cfg)? {
                bail!("Settings changed while connecting");
            }
            current.host_fingerprint = connected.fingerprint.clone();
            current.save()?;
            drop(current);
            *remote = Some(Arc::new(connected));
            ctl.wake();
            return Ok(());
        }
        *remote = Some(Arc::new(connected));
    }
    let remote = remote.as_ref().unwrap().clone();
    {
        let mut shared = ctl.shared.lock().unwrap();
        shared.conn = format!("{}@{}", cfg.user, cfg.host);
        shared.connected = true;
    }
    let cancel = || ctl.shutdown.load(Ordering::SeqCst);
    let mut snapshot = remote
        .tree(&cfg.remote_root, cfg.rules.recursive, &cancel)
        .await?;
    snapshot.context = source_context(cfg);
    validate_paths(snapshot.files.iter().map(|f| f.name.as_str()))?;
    *ctl.snapshot.lock().unwrap() = Some(snapshot.clone());
    let mut state = FileState::load(&cfg.local_root)?;
    state.check_source(&snapshot.context)?;
    if state.source.as_ref() != Some(&snapshot.context) {
        state.observations.clear();
        state.missing_since.clear();
        state.errors.clear();
    }
    state.source = Some(snapshot.context.clone());
    let now = chrono::Utc::now().timestamp();
    state.observe(&snapshot, now);
    state.save(&cfg.local_root)?;
    let local = local_files(cfg, &snapshot, &state)?;
    let forced = ctl.force_vm.lock().unwrap().clone();
    let running = ctl.running.load(Ordering::SeqCst);
    let items = plan(
        cfg,
        &snapshot,
        &state,
        &local,
        now,
        forced.as_deref(),
        running,
    )?;
    views(ctl, cfg, &snapshot, &state, &local, &items);
    ctl.repaint();
    let mut run = Run {
        at: now,
        copied: 0,
        removed: 0,
        bytes: 0,
        error: None,
    };
    let sources = snapshot
        .files
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect::<BTreeMap<_, _>>();
    for item in &items {
        if ctl.shutdown.load(Ordering::SeqCst) {
            break;
        }
        if !matches!(
            item.action,
            Action::Copy | Action::Delete | Action::Quarantine | Action::Forget
        ) {
            continue;
        }
        if ctl.snoozed.lock().unwrap().contains(&item.name)
            || ctl
                .retry
                .lock()
                .unwrap()
                .get(&item.name)
                .is_some_and(|(until, _)| *until > chrono::Utc::now().timestamp())
        {
            continue;
        }
        let manual = forced.as_deref() == Some(item.name.as_str());
        if !ctl.running.load(Ordering::SeqCst) && !manual {
            break;
        }
        // A settings edit invalidates the plan. Never publish work from an older context.
        if serde_json::to_vec(&*ctl.config.lock().unwrap())? != serde_json::to_vec(cfg)? {
            break;
        }
        ctl.cancel.store(false, Ordering::SeqCst);
        ctl.set_activity(Some(Activity {
            vm: item.name.clone(),
            phase: item.action.label().into(),
            file: item.name.clone(),
            started: chrono::Utc::now().timestamp(),
            ..Default::default()
        }));
        let result = async {
            if item.action == Action::Copy {
                let source = sources[item.name.as_str()];
                if fs2::available_space(root)?
                    < source
                        .size
                        .saturating_add(cfg.min_free_gb.saturating_mul(1 << 30))
                {
                    bail!("Insufficient space for staged update and reserve");
                }
                let (staged, side) = crate::sync::transfer_file(
                    ctl,
                    cfg,
                    remote.clone(),
                    &source.transfer_version(),
                    manual,
                )
                .await?;
                if remote
                    .stat_file(&cfg.remote_root, &source.name, &cancel)
                    .await?
                    .as_ref()
                    != Some(source)
                {
                    bail!("Source changed; staged copy was not committed");
                }
                let current = ctl.config.lock().unwrap();
                if ctl.shutdown.load(Ordering::SeqCst)
                    || ctl.cancel.load(Ordering::SeqCst)
                    || serde_json::to_vec(&*current)? != serde_json::to_vec(cfg)?
                {
                    bail!("Settings changed or operation cancelled");
                }
                // Recheck the local version immediately before publishing.
                if local_stamp(&target(root, &source.name)?)?.as_ref() != local.get(&source.name) {
                    bail!("Local file changed during transfer");
                }
                filetime::set_file_mtime(
                    &staged,
                    filetime::FileTime::from_unix_time(source.mtime, 0),
                )?;
                let stamp = install_file(cfg, &source.name, &staged)?;
                state.record(
                    &cfg.local_root,
                    &source.name,
                    Some(Record {
                        source: source.clone(),
                        local: stamp,
                        at: chrono::Utc::now().timestamp(),
                    }),
                )?;
                let committed_side =
                    target(&cache_dir(cfg).join("blk"), &format!("{}.blk", source.name))?;
                fs::create_dir_all(committed_side.parent().unwrap())?;
                fs::rename(side, committed_side)?;
                run.copied += 1;
                run.bytes += ctl
                    .shared
                    .lock()
                    .unwrap()
                    .activity
                    .as_ref()
                    .map(|a| a.net_bytes)
                    .unwrap_or(0);
            } else {
                // Reconfirm source absence; a file may have reappeared since the scan.
                if remote
                    .stat_file(&cfg.remote_root, &item.name, &cancel)
                    .await?
                    .is_some()
                {
                    bail!("Source reappeared; deletion cancelled");
                }
                let current = ctl.config.lock().unwrap();
                if ctl.shutdown.load(Ordering::SeqCst)
                    || ctl.cancel.load(Ordering::SeqCst)
                    || serde_json::to_vec(&*current)? != serde_json::to_vec(cfg)?
                {
                    bail!("Settings changed or operation cancelled");
                }
                if local_stamp(&target(root, &item.name)?)?.as_ref() != local.get(&item.name) {
                    bail!("Local copy changed before deletion");
                }
                remove_file(cfg, &mut state, &item.name, item.action)?;
                run.removed += 1;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            state.errors.insert(item.name.clone(), error.to_string());
            ctl.log(format!("✖ {}: {}", item.name, error));
            run.error = Some(error.to_string());
            ctl.retry
                .lock()
                .unwrap()
                .insert(item.name.clone(), (chrono::Utc::now().timestamp() + 60, 1));
        } else {
            state.errors.remove(&item.name);
            ctl.retry.lock().unwrap().remove(&item.name);
            ctl.log(format!("✔ {}: {}", item.name, item.action.label()));
        }
        if manual {
            *ctl.force_vm.lock().unwrap() = None;
        }
        ctl.set_activity(None);
    }
    if run.copied > 0 || run.removed > 0 || run.error.is_some() {
        state.history.push(run);
        if state.history.len() > 100 {
            state.history.remove(0);
        }
    }
    state.save(&cfg.local_root)?;
    let local = local_files(cfg, &snapshot, &state)?;
    let items = plan(
        cfg,
        &snapshot,
        &state,
        &local,
        chrono::Utc::now().timestamp(),
        None,
        ctl.running.load(Ordering::SeqCst),
    )?;
    views(ctl, cfg, &snapshot, &state, &local, &items);
    ctl.shared.lock().unwrap().waiting = i18n::choose(
        "İzleniyor · sonraki tarama bekleniyor",
        "Watching · waiting for the next scan",
    )
    .into();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::UpdateMode, rules::ProfileKind};
    fn file(name: &str, ns: i64) -> SourceFile {
        SourceFile {
            name: name.into(),
            size: 4,
            mtime: 1,
            mtime_ns: ns,
        }
    }
    fn config(root: &Path) -> Config {
        Config {
            kind: ProfileKind::Files,
            mode: UpdateMode::Safe,
            local_root: root.to_string_lossy().into(),
            min_free_gb: 0,
            ..Config::default()
        }
    }
    #[test]
    fn transfer_revisions_and_cache_namespaces_are_source_specific() {
        let first = file("one.bin", 1_000_000_001);
        let second = file("one.bin", 1_000_000_002);
        assert_eq!(first.remote().mtime, second.remote().mtime);
        assert_ne!(
            first.transfer_version().mtime,
            second.transfer_version().mtime
        );
        let mut cfg = config(Path::new(r"C:\example"));
        cfg.host = "one.example.invalid".into();
        let first = cache_dir(&cfg);
        cfg.host = "two.example.invalid".into();
        assert_ne!(first, cache_dir(&cfg));
        let second = cache_dir(&cfg);
        cfg.password_enc = "ciphertext-fixture".into();
        assert_eq!(second, cache_dir(&cfg));
    }
    #[test]
    fn deletions_obey_quiet_time_and_manual_plans() {
        let mut cfg = config(Path::new(r"C:\example"));
        cfg.rules.deletion = Deletion::Mirror;
        cfg.rules.quiet_seconds = 10;
        let local_stamp = LocalStamp {
            size: 4,
            modified_ns: 1,
        };
        let mut state = FileState::default();
        state.records.insert(
            "old.txt".into(),
            Record {
                source: file("old.txt", 1),
                local: local_stamp.clone(),
                at: 1,
            },
        );
        let snapshot = Snapshot::default();
        state.observe(&snapshot, 100);
        let local = BTreeMap::from([("old.txt".into(), local_stamp)]);
        assert_eq!(
            plan(&cfg, &snapshot, &state, &local, 109, None, true).unwrap()[0].action,
            Action::Wait
        );
        assert_eq!(
            plan(&cfg, &snapshot, &state, &local, 110, None, true).unwrap()[0].action,
            Action::Delete
        );
        cfg.plan = Plan::Manual;
        assert_eq!(
            plan(&cfg, &snapshot, &state, &local, 110, None, true).unwrap()[0].action,
            Action::Wait
        );
        assert_eq!(
            plan(&cfg, &snapshot, &state, &local, 110, Some("old.txt"), false).unwrap()[0].action,
            Action::Delete
        );
    }
    #[test]
    fn different_source_cannot_claim_existing_managed_state() {
        let mut state = FileState {
            source: Some("source-a".into()),
            ..Default::default()
        };
        state.records.insert(
            "file.txt".into(),
            Record {
                source: file("file.txt", 1),
                local: LocalStamp {
                    size: 4,
                    modified_ns: 1,
                },
                at: 1,
            },
        );
        assert!(state.check_source("source-b").is_err());
        assert!(state.check_source("source-a").is_ok());
    }
    #[test]
    fn plans_quiet_time_changes_protection_and_exclusions() {
        let mut cfg = config(Path::new(r"C:\example"));
        cfg.rules.quiet_seconds = 10;
        let snapshot = Snapshot {
            files: vec![file("deep/file.txt", 1), file("ignored.tmp", 1)],
            ..Default::default()
        };
        let mut state = FileState::default();
        state.observe(&snapshot, 100);
        let local = BTreeMap::new();
        let first = plan(&cfg, &snapshot, &state, &local, 109, None, true).unwrap();
        assert_eq!(first[0].action, Action::Wait);
        assert_eq!(first[1].action, Action::Excluded);
        assert_eq!(
            plan(&cfg, &snapshot, &state, &local, 110, None, true).unwrap()[0].action,
            Action::Copy
        );
        let mut changed = snapshot.clone();
        changed.files[0].mtime_ns = 2;
        state.observe(&changed, 110);
        assert_eq!(
            plan(&cfg, &changed, &state, &local, 111, None, true).unwrap()[0].action,
            Action::Wait
        );
        let mut local = BTreeMap::new();
        local.insert(
            "deep/file.txt".into(),
            LocalStamp {
                size: 4,
                modified_ns: 0,
            },
        );
        assert_eq!(
            plan(&cfg, &snapshot, &state, &local, 130, None, true).unwrap()[0].action,
            Action::Conflict
        );
    }
    #[test]
    fn ownership_limits_deletion_and_replay_survives_torn_tail() {
        let root = std::env::temp_dir().join(format!("omb-file-state-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let mut cfg = config(&root);
        cfg.rules.deletion = Deletion::Quarantine;
        cfg.rules.quiet_seconds = 0;
        let path = root.join("managed.txt");
        fs::write(&path, b"data").unwrap();
        fs::write(root.join("unmanaged.txt"), b"keep").unwrap();
        let mut state = FileState::default();
        state
            .record(
                &cfg.local_root,
                "managed.txt",
                Some(Record {
                    source: file("managed.txt", 1),
                    local: local_stamp(&path).unwrap().unwrap(),
                    at: 1,
                }),
            )
            .unwrap();
        let mut journal = OpenOptions::new()
            .append(true)
            .open(meta(&cfg.local_root).join("journal.jsonl"))
            .unwrap();
        journal.write_all(b"{torn").unwrap();
        drop(journal);
        let mut restored = FileState::load(&cfg.local_root).unwrap();
        assert!(restored.records.contains_key("managed.txt"));
        restored.save(&cfg.local_root).unwrap();
        let snapshot = Snapshot::default();
        let local = local_files(&cfg, &snapshot, &restored).unwrap();
        let items = plan(&cfg, &snapshot, &restored, &local, 100, None, true).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].action, Action::Quarantine);
        remove_file(&cfg, &mut restored, "managed.txt", Action::Quarantine).unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read(root.join("unmanaged.txt")).unwrap(), b"keep");
        assert!(remove_file(&cfg, &mut restored, "unmanaged.txt", Action::Delete).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn commits_replace_atomically_and_retain_previous_content() {
        let root = std::env::temp_dir().join(format!("omb-file-commit-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let cfg = config(&root);
        fs::write(root.join("one.txt"), b"old").unwrap();
        let staged = root.join("stage");
        fs::write(&staged, b"new").unwrap();
        install_file(&cfg, "one.txt", &staged).unwrap();
        assert_eq!(fs::read(root.join("one.txt")).unwrap(), b"new");
        let directory = meta(&cfg.local_root).join("versions/one.txt");
        let backup = fs::read_dir(directory)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::read(backup).unwrap(), b"old");
        fs::remove_dir_all(root).unwrap();
    }
}
