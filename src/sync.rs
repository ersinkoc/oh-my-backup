//! Background engine: scans the server, decides what/when to sync, transfers with block-level
//! delta + resume, commits a consistent set and removes old local copies.

use crate::blk::{BLOCK, Blk, ZERO, block_count};
use crate::config::{Config, FileRec, Plan, State, UpdateMode, app_dir};
use crate::i18n;
use crate::remote::{
    BlockHash, Cancelled, Remote, RemoteDir, RemoteFile, RemoteListing, hash_block,
};
use crate::schedule::{self, JobInfo};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BATCH: usize = 16; // blocks between sidecar checkpoints (64 MiB)
/// Files smaller than this are simply downloaded again: a delta round trip costs more than it saves.
const DELTA_MIN: u64 = 64 << 20;
/// Back-off after consecutive failures of the same dir (seconds).
const RETRY_STEPS: [i64; 5] = [60, 300, 900, 1800, 3600];

// ---------------------------------------------------------------- shared state for the GUI

#[derive(Clone, Debug)]
pub struct LocalFile {
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct VmView {
    pub name: String,
    pub remote: Vec<RemoteFile>,
    pub remote_size: u64,
    pub busy: bool,
    pub job: Option<JobInfo>,
    pub local: Vec<LocalFile>,
    pub local_size: u64,
    pub local_exists: bool,
    pub committed_at: Option<i64>,
    pub committed: Vec<FileRec>,
    pub in_sync: bool,
    pub incomplete: bool,
    pub excluded: bool,
    pub legacy: Vec<(String, u64)>,
    pub staging_files: Vec<LocalFile>,
    pub status: String,
    pub last_error: Option<String>,
    pub est_bytes: u64,
    pub plan: Plan,
    pub plan_custom: bool,
    /// when the plan makes this dir due again: None = manual only, i64::MIN = now
    pub due_at: Option<i64>,
    pub retry_at: Option<i64>,
    pub snoozed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Activity {
    pub vm: String,
    pub phase: String,
    pub file: String,
    pub done: u64,
    pub total: u64,
    pub speed: f64,
    pub started: i64,
    pub net_bytes: u64,
    /// background work running in parallel (server-side hash of the .vbk)
    pub bg: String,
}

#[derive(Default)]
pub struct Shared {
    pub conn: String,
    pub connected: bool,
    pub remote_disk: Option<(u64, u64)>,
    pub last_scan: Option<i64>,
    pub vms: Vec<VmView>,
    pub others: Vec<(String, u64)>,
    pub activity: Option<Activity>,
    pub waiting: String,
    pub log: VecDeque<String>,
    pub local_free: u64,
    pub local_total: u64,
    /// bytes received per second, newest last (max 1 h)
    pub speed_hist: VecDeque<f32>,
    pub session_start: i64,
    pub session_bytes: u64,
    pub session_active_s: u64,
    pub stats: crate::stats::Stats,
}

pub struct Control {
    pub shutdown: AtomicBool,
    pub engine_stopped: AtomicBool,
    pub sampler_stopped: AtomicBool,
    pub running: AtomicBool,
    pub cancel: AtomicBool,
    pub scan_now: AtomicBool,
    pub reconnect: AtomicBool,
    pub force_vm: Mutex<Option<String>>,
    pub notify: tokio::sync::Notify,
    pub config: Mutex<Config>,
    pub state: Mutex<State>,
    /// local_root the in-memory `state` belongs to
    pub state_root: Mutex<String>,
    pub shared: Mutex<Shared>,
    pub ctx: Mutex<Option<eframe::egui::Context>>,
    pub speed_est: Mutex<f64>,
    /// every byte received from the server (sampled once per second for the speed graph)
    pub net_total: AtomicU64,
    /// headless test mode: run until nothing is left to do, then return
    pub once: AtomicBool,
    /// dir -> (retry not before, consecutive failures)
    pub retry: Mutex<HashMap<String, (i64, u32)>>,
    /// Dirs stopped by the user; skipped until manually resumed.
    pub snoozed: Mutex<HashSet<String>>,
}

impl Control {
    pub fn new(cfg: Config) -> Arc<Control> {
        let state = State::load(&cfg.local_root);
        Arc::new(Control {
            shutdown: AtomicBool::new(false),
            engine_stopped: AtomicBool::new(false),
            sampler_stopped: AtomicBool::new(false),
            running: AtomicBool::new(cfg.auto_start),
            cancel: AtomicBool::new(false),
            scan_now: AtomicBool::new(true),
            reconnect: AtomicBool::new(false),
            force_vm: Mutex::new(None),
            notify: tokio::sync::Notify::new(),
            state_root: Mutex::new(cfg.local_root.clone()),
            config: Mutex::new(cfg),
            state: Mutex::new(state),
            shared: Mutex::new(Shared {
                session_start: chrono::Utc::now().timestamp(),
                stats: crate::stats::Stats::load(),
                ..Default::default()
            }),
            ctx: Mutex::new(None),
            speed_est: Mutex::new(15e6),
            net_total: AtomicU64::new(0),
            once: AtomicBool::new(false),
            retry: Mutex::new(HashMap::new()),
            snoozed: Mutex::new(HashSet::new()),
        })
    }

    pub fn repaint(&self) {
        if let Some(c) = self.ctx.lock().unwrap().as_ref() {
            c.request_repaint();
        }
    }

    pub fn log(&self, msg: impl AsRef<str>) {
        let line = format!(
            "{} {}",
            chrono::Local::now().format("%d.%m %H:%M:%S"),
            msg.as_ref()
        );
        {
            let mut s = self.shared.lock().unwrap();
            s.log.push_back(line.clone());
            while s.log.len() > 3000 {
                s.log.pop_front();
            }
        }
        if self.once.load(Ordering::Relaxed) {
            println!("{line}");
        }
        let dir = app_dir().join("logs");
        let _ = fs::create_dir_all(&dir);
        let p = dir.join(format!(
            "hbsync-{}.log",
            chrono::Local::now().format("%Y-%m")
        ));
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{line}");
        }
        self.repaint();
    }

    fn set_activity(&self, a: Option<Activity>) {
        self.shared.lock().unwrap().activity = a;
        self.repaint();
    }

    fn update_activity(&self, f: impl FnOnce(&mut Activity)) {
        if let Some(a) = self.shared.lock().unwrap().activity.as_mut() {
            f(a);
        }
        self.repaint();
    }

    fn save_state(&self) {
        let root = self.state_root.lock().unwrap().clone();
        if let Err(e) = self.state.lock().unwrap().save(&root) {
            self.log(format!("durum kaydedilemedi: {e:#}"));
        }
    }

    pub fn wake(&self) {
        self.notify.notify_one();
    }

    /// Start a dir by hand: clears snooze/back-off and makes it the next job.
    pub fn force(&self, name: &str) {
        self.snoozed.lock().unwrap().remove(name);
        self.retry.lock().unwrap().remove(name);
        *self.force_vm.lock().unwrap() = Some(name.to_string());
        self.scan_now.store(true, Ordering::SeqCst);
        self.wake();
    }

    /// Stop the running job; that dir is not picked up again automatically until the user says so.
    pub fn stop_current(&self) {
        let cur = self
            .shared
            .lock()
            .unwrap()
            .activity
            .as_ref()
            .map(|a| a.vm.clone());
        if let Some(vm) = cur {
            self.snoozed.lock().unwrap().insert(vm);
        }
        *self.force_vm.lock().unwrap() = None;
        self.cancel.store(true, Ordering::SeqCst);
        self.scan_now.store(true, Ordering::SeqCst);
        self.wake();
    }
}

/// Samples `net_total` once per second into the speed history and the persistent statistics.
pub fn start_sampler(ctl: Arc<Control>) {
    std::thread::spawn(move || {
        let mut last = ctl.net_total.load(Ordering::Relaxed);
        let mut next = Instant::now();
        let mut last_save = Instant::now();
        let mut dirty = false;
        loop {
            next += Duration::from_secs(1);
            let n = Instant::now();
            if next > n {
                std::thread::sleep(next - n);
            } else {
                next = n;
            }
            let cur = ctl.net_total.load(Ordering::Relaxed);
            let d = cur - last;
            last = cur;
            let mut s = ctl.shared.lock().unwrap();
            s.speed_hist.push_back(d as f32);
            while s.speed_hist.len() > 3600 {
                s.speed_hist.pop_front();
            }
            if d > 0 {
                s.session_bytes += d;
                s.session_active_s += 1;
                s.stats.add(d);
                dirty = true;
            }
            if ctl.shutdown.load(Ordering::SeqCst) {
                let stats = s.stats.clone();
                drop(s);
                stats.save();
                ctl.sampler_stopped.store(true, Ordering::SeqCst);
                ctl.repaint();
                return;
            }
            if dirty && last_save.elapsed() > Duration::from_secs(30) {
                let st = s.stats.clone();
                drop(s);
                st.save();
                dirty = false;
                last_save = Instant::now();
            }
        }
    });
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn recs(files: &[RemoteFile]) -> Vec<FileRec> {
    files
        .iter()
        .map(|f| FileRec {
            name: f.name.clone(),
            size: f.size,
            mtime: f.mtime,
        })
        .collect()
}

fn same_rec(c: &FileRec, f: &RemoteFile) -> bool {
    c.name == f.name && c.size == f.size && c.mtime == f.mtime
}

fn local_matches(local: &[LocalFile], name: &str, size: u64) -> bool {
    local.iter().any(|l| l.name == name && l.size == size)
}

fn set_current(
    committed: &[FileRec],
    remote: &[RemoteFile],
    local: &[LocalFile],
    incomplete: bool,
) -> bool {
    !incomplete
        && !committed.is_empty()
        && committed == recs(remote)
        && remote.iter().all(|f| local_matches(local, &f.name, f.size))
}

/// VM key for matching legacy local dirs: strip _HELSINKI / _NODE01 / _NODE01_1 suffixes.
pub fn vm_key(name: &str) -> String {
    let mut s = name.to_uppercase();
    loop {
        let before = s.clone();
        s = s.trim_end_matches('_').to_string();
        if let Some(x) = s.strip_suffix("_HELSINKI") {
            s = x.to_string();
        }
        // _NODE<digits> optionally followed by _<digits>
        if let Some(pos) = s.rfind("_NODE") {
            let tail = &s[pos + 5..];
            let ok = {
                let mut parts = tail.splitn(2, '_');
                let a = parts.next().unwrap_or("");
                let b = parts.next();
                !a.is_empty()
                    && a.chars().all(|c| c.is_ascii_digit())
                    && b.is_none_or(|b| !b.is_empty() && b.chars().all(|c| c.is_ascii_digit()))
            };
            if ok {
                s.truncate(pos);
            }
        }
        if s == before {
            return s;
        }
    }
}

fn list_local_files(dir: &Path) -> Vec<LocalFile> {
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(m) = e.metadata()
                && m.is_file()
            {
                v.push(LocalFile {
                    name: e.file_name().to_string_lossy().into(),
                    size: m.len(),
                });
            }
        }
    }
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

fn dir_size(dir: &Path) -> u64 {
    let mut t = 0;
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(m) = e.metadata() {
                if m.is_dir() {
                    t += dir_size(&e.path());
                } else {
                    t += m.len();
                }
            }
        }
    }
    t
}

fn is_vbk(n: &str) -> bool {
    n.to_ascii_lowercase().ends_with(".vbk")
}

fn file_order(n: &str) -> u8 {
    let l = n.to_ascii_lowercase();
    if l.ends_with(".vib") || l.ends_with(".vrb") {
        0
    } else if l.ends_with(".vbk") {
        1
    } else {
        2
    }
}

// ---------------------------------------------------------------- views

fn refresh_views(ctl: &Control, cfg: &Config, l: &RemoteListing) {
    let root = PathBuf::from(&cfg.local_root);
    let meta = cfg.meta_dir();
    let state = ctl.state.lock().unwrap().clone();
    let retry = ctl.retry.lock().unwrap().clone();
    let snoozed = ctl.snoozed.lock().unwrap().clone();
    let mut local_dirs: Vec<String> = fs::read_dir(&root)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    local_dirs.sort();
    let remote_names: HashSet<&str> = l.dirs.iter().map(|d| d.name.as_str()).collect();
    let mut used = HashSet::new();
    let mut sizes: BTreeMap<String, u64> = BTreeMap::new();
    let mut size_of = |n: &str| {
        *sizes
            .entry(n.to_string())
            .or_insert_with(|| dir_size(&root.join(n)))
    };
    let speed = *ctl.speed_est.lock().unwrap();
    let t = now();

    let mut vms = Vec::new();
    for d in l.dirs.iter().filter(|d| !d.files.is_empty()) {
        let st = state.vms.get(&d.name).cloned().unwrap_or_default();
        let job = schedule::estimate(&d.files);
        let local = list_local_files(&root.join(&d.name));
        let committed = st.committed.clone().unwrap_or_default();
        let in_sync = set_current(&committed, &d.files, &local, st.live_set_incomplete);
        let key = vm_key(&d.name);
        let legacy: Vec<(String, u64)> = local_dirs
            .iter()
            .filter(|n| **n != d.name && !remote_names.contains(n.as_str()) && vm_key(n) == key)
            .map(|n| (n.clone(), size_of(n)))
            .collect();
        used.insert(d.name.clone());
        for (n, _) in &legacy {
            used.insert(n.clone());
        }
        let excluded = cfg.excluded.contains(&d.name);
        let had_vbk = committed.iter().any(|c| is_vbk(&c.name));
        let est: u64 = d
            .files
            .iter()
            .filter(|f| {
                !committed.iter().any(|c| same_rec(c, f)) || !local_matches(&local, &f.name, f.size)
            })
            .map(|f| {
                if f.size >= DELTA_MIN
                    && is_vbk(&f.name)
                    && had_vbk
                    && local.iter().any(|l| is_vbk(&l.name))
                {
                    f.size / 10
                } else {
                    f.size
                }
            })
            .sum();
        let plan = cfg.plan_for(&d.name);
        let due_at = schedule::plan_due_at(plan, cfg.plan_hour, st.committed_at);
        let retry_at = retry.get(&d.name).map(|r| r.0).filter(|u| *u > t);
        let is_snoozed = snoozed.contains(&d.name);
        let next_plan = due_at.filter(|u| *u > t && plan != Plan::Continuous);
        let status = if excluded {
            i18n::text("Hariç tutuldu").to_string()
        } else if in_sync {
            match next_plan {
                Some(u) => i18n::choose(
                    format!("Güncel ✓ · sonraki plan {}", schedule::fmt_time(u)),
                    format!("Up to date ✓ · next scheduled {}", schedule::fmt_time(u)),
                ),
                None => i18n::text("Güncel ✓").to_string(),
            }
        } else if is_snoozed {
            i18n::text("Durduruldu — 'Şimdi senkronla' ya da 'Başlat' ile devam").to_string()
        } else if let Some(u) = retry_at {
            i18n::choose(
                format!(
                    "Hata sonrası bekleniyor, {} tekrar denenecek",
                    schedule::hm(u)
                ),
                format!("Waiting after error, retry at {}", schedule::hm(u)),
            )
        } else if due_at.is_none() && !st.live_set_incomplete {
            i18n::text("Sadece elle — 'Şimdi senkronla'").to_string()
        } else if let Some(u) = next_plan.filter(|_| !st.live_set_incomplete) {
            i18n::choose(
                format!("Planlı: {}", schedule::fmt_time(u)),
                format!("Scheduled: {}", schedule::fmt_time(u)),
            )
        } else if d.busy {
            i18n::text("Sunucuda job çalışıyor").to_string()
        } else if let Some(u) = job
            .as_ref()
            .and_then(|j| j.blocked_until(t, cfg.margin_before_min * 60, cfg.margin_after_min * 60))
        {
            i18n::choose(
                format!("Job penceresi, {} sonrası", schedule::hm(u)),
                format!("Job window, after {}", schedule::hm(u)),
            )
        } else if cfg.hours_limit && !schedule::hours_allowed(t, cfg.hours_start, cfg.hours_end) {
            i18n::choose(
                format!("İzinli saat dışında ({}:00 bekleniyor)", cfg.hours_start),
                format!("Outside allowed hours (waiting for {}:00)", cfg.hours_start),
            )
        } else {
            i18n::choose(
                format!(
                    "İndirilecek (~{}, ~{})",
                    human(est),
                    dur_h((est as f64 / speed) as i64)
                ),
                format!(
                    "Pending download (~{}, ~{})",
                    human(est),
                    dur_h((est as f64 / speed) as i64)
                ),
            )
        };
        vms.push(VmView {
            name: d.name.clone(),
            remote_size: d.files.iter().map(|f| f.size).sum(),
            remote: d.files.clone(),
            busy: d.busy,
            job,
            local_size: local.iter().map(|f| f.size).sum(),
            local_exists: root.join(&d.name).exists(),
            local,
            committed_at: st.committed_at,
            committed,
            in_sync,
            incomplete: st.live_set_incomplete,
            excluded,
            legacy,
            staging_files: list_local_files(&meta.join("staging").join(&d.name)),
            status,
            last_error: st.last_error.clone(),
            est_bytes: est,
            plan,
            plan_custom: cfg.vm_plan.contains_key(&d.name),
            due_at,
            retry_at,
            snoozed: is_snoozed,
        });
    }
    let others: Vec<(String, u64)> = local_dirs
        .iter()
        .filter(|n| !used.contains(*n))
        .map(|n| (n.clone(), size_of(n)))
        .collect();
    let (free, total) = (
        fs2::available_space(&root).unwrap_or(0),
        fs2::total_space(&root).unwrap_or(0),
    );
    let mut s = ctl.shared.lock().unwrap();
    s.vms = vms;
    s.others = others;
    s.remote_disk = Some((l.disk_total, l.disk_free));
    s.last_scan = Some(t);
    s.local_free = free;
    s.local_total = total;
    drop(s);
    ctl.repaint();
}

pub fn human(b: u64) -> String {
    let b = b as f64;
    if b >= 1e12 {
        format!("{:.2} TB", b / 1e12)
    } else if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} KB", b / 1e3)
    }
}

pub fn dur_h(s: i64) -> String {
    if s >= 2 * 86_400 {
        i18n::choose(
            format!("{}g {}s", s / 86_400, (s % 86_400) / 3600),
            format!("{}d {}h", s / 86_400, (s % 86_400) / 3600),
        )
    } else if s >= 3600 {
        i18n::choose(
            format!("{}s {}dk", s / 3600, (s % 3600) / 60),
            format!("{}h {}m", s / 3600, (s % 3600) / 60),
        )
    } else if s >= 60 {
        i18n::choose(format!("{}dk", s / 60), format!("{}m", s / 60))
    } else {
        i18n::choose(format!("{}sn", s.max(0)), format!("{}s", s.max(0)))
    }
}

// ---------------------------------------------------------------- main loop

enum Pick {
    Vm(String),
    Wait(i64, String),
}

fn pick(ctl: &Control, cfg: &Config, l: &RemoteListing) -> Pick {
    let t = now();
    let running = ctl.running.load(Ordering::SeqCst);
    let forced = ctl.force_vm.lock().unwrap().clone();
    let state = ctl.state.lock().unwrap().clone();
    let retry = ctl.retry.lock().unwrap().clone();
    let snoozed = ctl.snoozed.lock().unwrap().clone();
    let speed = *ctl.speed_est.lock().unwrap();
    let views = ctl.shared.lock().unwrap().vms.clone();
    let (before, after) = (cfg.margin_before_min * 60, cfg.margin_after_min * 60);
    let mut cands = Vec::new();
    let mut waits: Vec<(i64, String)> = Vec::new();
    if let Some(f) = &forced
        && !l.dirs.iter().any(|d| d.name == *f && !d.files.is_empty())
    {
        ctl.log(i18n::choose(
            format!("{f}: sunucuda yok ya da boş, elle başlatma iptal"),
            format!("{f}: missing or empty on server; manual start cancelled"),
        ));
        *ctl.force_vm.lock().unwrap() = None;
    }
    for d in l.dirs.iter().filter(|d| !d.files.is_empty()) {
        let is_forced = forced.as_deref() == Some(d.name.as_str());
        let st = state.vms.get(&d.name).cloned().unwrap_or_default();
        let committed = st.committed.clone().unwrap_or_default();
        let local = list_local_files(&Path::new(&cfg.local_root).join(&d.name));
        let damaged = committed
            .iter()
            .any(|f| !local_matches(&local, &f.name, f.size));
        if set_current(&committed, &d.files, &local, st.live_set_incomplete) {
            if is_forced {
                ctl.log(i18n::choose(
                    format!("{}: zaten güncel, indirilecek bir şey yok", d.name),
                    format!("{}: already up to date, nothing to download", d.name),
                ));
                *ctl.force_vm.lock().unwrap() = None;
            }
            continue;
        }
        if !is_forced {
            // paused: only an explicitly started dir runs
            if !running || cfg.excluded.contains(&d.name) || snoozed.contains(&d.name) {
                continue;
            }
            if let Some(&(until, _)) = retry.get(&d.name)
                && until > t
            {
                waits.push((
                    until,
                    i18n::choose(
                        format!("{}: hata sonrası yeniden deneme", d.name),
                        format!("{}: retry after error", d.name),
                    ),
                ));
                continue;
            }
            // a half-updated local set (in-place mode) is always finished, plan or not
            if !st.live_set_incomplete && !damaged {
                match schedule::plan_due_at(cfg.plan_for(&d.name), cfg.plan_hour, st.committed_at) {
                    None => continue,
                    Some(due) if due > t => {
                        waits.push((
                            due,
                            i18n::choose(
                                format!("{}: planlı ({})", d.name, schedule::fmt_time(due)),
                                format!("{}: scheduled ({})", d.name, schedule::fmt_time(due)),
                            ),
                        ));
                        continue;
                    }
                    _ => {}
                }
            }
        }
        let job = schedule::estimate(&d.files);
        if d.busy {
            waits.push((
                t + 300,
                i18n::choose(
                    format!("{}: sunucuda job çalışıyor", d.name),
                    format!("{}: server job is running", d.name),
                ),
            ));
            continue;
        }
        if let Some(u) = job.as_ref().and_then(|j| j.blocked_until(t, before, after)) {
            waits.push((
                u,
                i18n::choose(
                    format!("{}: job penceresi ({} sonrası)", d.name, schedule::hm(u)),
                    format!("{}: job window (after {})", d.name, schedule::hm(u)),
                ),
            ));
            continue;
        }
        if cfg.hours_limit
            && !schedule::hours_allowed(t, cfg.hours_start, cfg.hours_end)
            && !is_forced
        {
            let u = schedule::hours_next_open(t, cfg.hours_start);
            waits.push((
                u,
                i18n::choose(
                    format!("izinli saat dışında ({}:00 bekleniyor)", cfg.hours_start),
                    format!("outside allowed hours (waiting for {}:00)", cfg.hours_start),
                ),
            ));
            continue;
        }
        let est = views
            .iter()
            .find(|v| v.name == d.name)
            .map(|v| v.est_bytes)
            .unwrap_or(0);
        let next = job
            .as_ref()
            .map(|j| j.next_block(t, before, after))
            .unwrap_or(t + 86400);
        let fits = t + (est as f64 / speed) as i64 + 600 < next;
        cands.push((!is_forced, !fits, est, d.name.clone()));
    }
    if !cands.is_empty() {
        cands.sort();
        return Pick::Vm(cands[0].3.clone());
    }
    if let Some(w) = waits.into_iter().min_by_key(|w| w.0) {
        return Pick::Wait(w.0, w.1);
    }
    let why = if running {
        i18n::text("Bekleyen iş yok")
    } else {
        i18n::text("Duraklatıldı")
    };
    Pick::Wait(t + cfg.scan_interval_min as i64 * 60, why.into())
}

async fn wait_cmd(ctl: &Control, d: Duration) {
    tokio::select! {
        _ = tokio::time::sleep(d) => {},
        _ = ctl.notify.notified() => {},
    }
}

/// Finish what an interrupted commit left in `.hbsync/trash`: restore a live set that was moved
/// away but never replaced, and delete old sets whose replacement is in place.
fn recover_trash(ctl: &Control, cfg: &Config) {
    let tdir = cfg.meta_dir().join("trash");
    let Ok(rd) = fs::read_dir(&tdir) else { return };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        let Some((vm, ts)) = n.rsplit_once('-') else {
            continue;
        };
        if vm.is_empty() || !ts.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let live = Path::new(&cfg.local_root).join(vm);
        if !live.exists() {
            if cfg.meta_dir().join("staging").join(vm).exists() {
                ctl.log(i18n::choose(format!("{vm}: yarım kalmış değişim — eski set {} korunuyor, yeni set bir sonraki senkronda yerine konacak", e.path().display()), format!("{vm}: interrupted swap — retaining old set {}; the next sync will install the new set", e.path().display())));
            } else {
                match fs::rename(e.path(), &live) {
                    Ok(()) => ctl.log(i18n::choose(
                        format!("{vm}: yarım kalmış değişimden eski set geri yüklendi"),
                        format!("{vm}: restored old set after interrupted swap"),
                    )),
                    Err(err) => ctl.log(i18n::choose(
                        format!("{vm}: eski set geri yüklenemedi: {err}"),
                        format!("{vm}: could not restore old set: {err}"),
                    )),
                }
            }
        } else if let Err(err) = fs::remove_dir_all(e.path()) {
            ctl.log(i18n::choose(
                format!("{vm}: çöp dizini silinemedi {}: {err}", e.path().display()),
                format!(
                    "{vm}: could not delete trash folder {}: {err}",
                    e.path().display()
                ),
            ));
        }
    }
}

struct EngineExit(Arc<Control>);
impl Drop for EngineExit {
    fn drop(&mut self) {
        self.0.engine_stopped.store(true, Ordering::SeqCst);
        self.0.repaint();
    }
}

pub async fn engine(ctl: Arc<Control>) {
    let _exit = EngineExit(ctl.clone());
    if ctl.shutdown.load(Ordering::SeqCst) {
        return;
    }
    let mut remote: Option<Arc<Remote>> = None;
    let mut listing: Option<RemoteListing> = None;
    let mut last_scan = 0i64;
    ctl.log(i18n::text("Motor başlatıldı"));
    let mut recovered_root = None;
    loop {
        if ctl.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let cfg = ctl.config.lock().unwrap().clone();
        if let Err(e) = cfg.validate() {
            ctl.shared.lock().unwrap().waiting = i18n::choose(
                format!("Ayarlar geçersiz: {e}"),
                format!("Invalid settings: {e}"),
            );
            ctl.repaint();
            if ctl.once.load(Ordering::SeqCst) {
                return;
            }
            wait_cmd(&ctl, Duration::from_secs(30)).await;
            continue;
        }
        if recovered_root.as_ref() != Some(&cfg.local_root) {
            recover_trash(&ctl, &cfg);
            recovered_root = Some(cfg.local_root.clone());
        }
        // local root changed in the settings: switch the state over (nothing is running here)
        if *ctl.state_root.lock().unwrap() != cfg.local_root {
            *ctl.state.lock().unwrap() = State::load(&cfg.local_root);
            *ctl.state_root.lock().unwrap() = cfg.local_root.clone();
            ctl.log(i18n::choose(
                format!("Yerel dizin değişti: {}", cfg.local_root),
                format!("Local folder changed: {}", cfg.local_root),
            ));
            recover_trash(&ctl, &cfg);
            ctl.scan_now.store(true, Ordering::SeqCst);
        }
        if ctl.reconnect.swap(false, Ordering::SeqCst)
            && let Some(r) = remote.take()
            && let Ok(r) = Arc::try_unwrap(r)
        {
            r.close().await;
        }
        if remote.as_ref().is_none_or(|r| r.is_closed()) {
            remote = None;
            {
                let mut s = ctl.shared.lock().unwrap();
                s.conn = i18n::choose(
                    format!("Bağlanıyor: {}@{}…", cfg.user, cfg.host),
                    format!("Connecting: {}@{}…", cfg.user, cfg.host),
                );
                s.connected = false;
            }
            ctl.repaint();
            let pw = cfg.password();
            if pw.is_empty() {
                ctl.shared.lock().unwrap().conn = i18n::text("Parola girilmemiş (Ayarlar)").into();
                ctl.repaint();
                if ctl.once.load(Ordering::SeqCst) {
                    ctl.log(i18n::text("[once] parola yok"));
                    return;
                }
                wait_cmd(&ctl, Duration::from_secs(30)).await;
                continue;
            }
            let fp = (!cfg.host_fingerprint.is_empty()).then(|| cfg.host_fingerprint.clone());
            match Remote::connect(&cfg.host, cfg.port, &cfg.user, &pw, fp).await {
                Ok(r) => {
                    if cfg.host_fingerprint.is_empty() {
                        let mut c = ctl.config.lock().unwrap();
                        c.host_fingerprint = r.fingerprint.clone();
                        let _ = c.save();
                    }
                    ctl.log(i18n::choose(
                        format!("Sunucuya bağlanıldı ({})", r.fingerprint),
                        format!("Connected to server ({})", r.fingerprint),
                    ));
                    let mut s = ctl.shared.lock().unwrap();
                    s.conn = i18n::choose(
                        format!("Bağlı: {}@{}", cfg.user, cfg.host),
                        format!("Connected: {}@{}", cfg.user, cfg.host),
                    );
                    s.connected = true;
                    drop(s);
                    remote = Some(Arc::new(r));
                    ctl.scan_now.store(true, Ordering::SeqCst);
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    ctl.log(i18n::choose(
                        format!("Bağlantı hatası: {msg}"),
                        format!("Connection error: {msg}"),
                    ));
                    ctl.shared.lock().unwrap().conn = i18n::choose(
                        format!("Bağlantı yok: {msg}"),
                        format!("Disconnected: {msg}"),
                    );
                    if ctl.once.load(Ordering::SeqCst) {
                        return;
                    }
                    ctl.repaint();
                    wait_cmd(&ctl, Duration::from_secs(60)).await;
                    continue;
                }
            }
        }
        if !Path::new(&cfg.local_root).is_absolute() || !cfg.remote_root.starts_with('/') {
            ctl.shared.lock().unwrap().waiting =
                i18n::text("Yerel/sunucu yedek dizini geçersiz (Ayarlar)").into();
            ctl.repaint();
            if ctl.once.load(Ordering::SeqCst) {
                ctl.log(i18n::text("[once] dizin ayarları geçersiz"));
                return;
            }
            wait_cmd(&ctl, Duration::from_secs(30)).await;
            continue;
        }
        let r = remote.clone().unwrap();

        let t = now();
        if ctl.scan_now.swap(false, Ordering::SeqCst)
            || listing.is_none()
            || t - last_scan >= cfg.scan_interval_min as i64 * 60
        {
            match r
                .list_cancellable(&cfg.remote_root, &|| ctl.shutdown.load(Ordering::SeqCst))
                .await
            {
                Ok(l) => {
                    let c2 = cfg.clone();
                    let ctl2 = ctl.clone();
                    let l2 = l.clone();
                    let _ =
                        tokio::task::spawn_blocking(move || refresh_views(&ctl2, &c2, &l2)).await;
                    listing = Some(l);
                    last_scan = t;
                }
                Err(e) => {
                    ctl.log(i18n::choose(
                        format!("Sunucu listelenemedi: {e:#}"),
                        format!("Could not list server: {e:#}"),
                    ));
                    remote = None;
                    if ctl.once.load(Ordering::SeqCst) {
                        return;
                    }
                    wait_cmd(&ctl, Duration::from_secs(20)).await;
                    continue;
                }
            }
        }

        match pick(&ctl, &cfg, listing.as_ref().unwrap()) {
            Pick::Wait(until, why) => {
                if ctl.once.load(Ordering::SeqCst) {
                    ctl.log(format!("[once] bitti: {why}"));
                    return;
                }
                let next = until.min(t + cfg.scan_interval_min as i64 * 60);
                ctl.shared.lock().unwrap().waiting =
                    format!("{why} — sonraki kontrol {}", schedule::hm(next));
                ctl.repaint();
                let d = (until - t).clamp(10, cfg.scan_interval_min as i64 * 60) as u64;
                wait_cmd(&ctl, Duration::from_secs(d)).await;
                ctl.scan_now.store(true, Ordering::SeqCst);
            }
            Pick::Vm(name) => {
                ctl.cancel.store(false, Ordering::SeqCst);
                ctl.shared.lock().unwrap().waiting = format!("{name} senkronize ediliyor");
                let started = now();
                ctl.log(i18n::choose(
                    format!("▶ {name}: senkron başlıyor"),
                    format!("▶ {name}: starting sync"),
                ));
                ctl.set_activity(Some(Activity {
                    vm: name.clone(),
                    phase: i18n::text("Hazırlanıyor").into(),
                    started,
                    ..Default::default()
                }));
                let res = sync_vm(&ctl, &cfg, r.clone(), &name).await;
                let net = ctl
                    .shared
                    .lock()
                    .unwrap()
                    .activity
                    .as_ref()
                    .map(|a| a.net_bytes)
                    .unwrap_or(0);
                ctl.set_activity(None);
                {
                    let mut f = ctl.force_vm.lock().unwrap();
                    if f.as_deref() == Some(name.as_str()) {
                        *f = None;
                    }
                }
                let mut wait_after = 0;
                match res {
                    Ok(Outcome::Committed) => {
                        let d = now() - started;
                        ctl.log(i18n::choose(
                            format!(
                                "✔ {name}: tamamlandı ({} indirildi, {})",
                                human(net),
                                dur_h(d)
                            ),
                            format!(
                                "✔ {name}: completed ({} downloaded, {})",
                                human(net),
                                dur_h(d)
                            ),
                        ));
                        ctl.retry.lock().unwrap().remove(&name);
                        let mut st = ctl.state.lock().unwrap();
                        let v = st.vms.entry(name.clone()).or_default();
                        v.last_duration_s = Some(d);
                        v.last_bytes = Some(net);
                        drop(st);
                        ctl.save_state();
                    }
                    Ok(Outcome::Interrupted(why, retry_in)) => {
                        ctl.log(i18n::choose(
                            format!("⏸ {name}: durduruldu — {why} (ilerleme korunuyor)"),
                            format!("⏸ {name}: stopped — {why} (progress preserved)"),
                        ));
                        if let Some(s) = retry_in {
                            ctl.retry
                                .lock()
                                .unwrap()
                                .insert(name.clone(), (now() + s, 0));
                        }
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        let delay = {
                            let mut rt = ctl.retry.lock().unwrap();
                            let fails = rt.get(&name).map(|x| x.1).unwrap_or(0) + 1;
                            let delay =
                                RETRY_STEPS[(fails as usize - 1).min(RETRY_STEPS.len() - 1)];
                            rt.insert(name.clone(), (now() + delay, fails));
                            delay
                        };
                        ctl.log(i18n::choose(
                            format!(
                                "✖ {name}: hata — {msg} ({} sonra tekrar denenecek)",
                                dur_h(delay)
                            ),
                            format!("✖ {name}: error — {msg} (retry in {})", dur_h(delay)),
                        ));
                        ctl.state
                            .lock()
                            .unwrap()
                            .vms
                            .entry(name.clone())
                            .or_default()
                            .last_error = Some(msg);
                        ctl.save_state();
                        if r.is_closed() {
                            remote = None;
                        }
                        if ctl.once.load(Ordering::SeqCst) {
                            return;
                        }
                        wait_after = 5;
                    }
                }
                ctl.scan_now.store(true, Ordering::SeqCst);
                if wait_after > 0 {
                    wait_cmd(&ctl, Duration::from_secs(wait_after)).await;
                }
            }
        }
    }
}

// ---------------------------------------------------------------- per-VM sync

enum Outcome {
    Committed,
    /// reason, optional "do not retry for N seconds"
    Interrupted(String, Option<i64>),
}

const STOP_NONE: u8 = 0;
const STOP_USER: u8 = 1;
const STOP_WINDOW: u8 = 2;
const STOP_BUSY: u8 = 3;
const STOP_HOURS: u8 = 4;

fn stop_reason(c: u8) -> &'static str {
    match c {
        STOP_USER => i18n::text("kullanıcı durdurdu"),
        STOP_WINDOW => i18n::text("job penceresi yaklaştı"),
        STOP_BUSY => i18n::text("sunucuda job başladı"),
        STOP_HOURS => i18n::text("izinli saat bitti"),
        _ => i18n::text("durduruldu"),
    }
}

struct Env<'a> {
    ctl: &'a Arc<Control>,
    cfg: &'a Config,
    r: Arc<Remote>,
    stop: Arc<AtomicU8>,
    name: &'a str,
}

impl Env<'_> {
    fn stopped(&self) -> bool {
        self.ctl.shutdown.load(Ordering::SeqCst) || self.stop.load(Ordering::SeqCst) != STOP_NONE
    }
    fn check(&self) -> Result<()> {
        if self.stopped() {
            Err(Cancelled.into())
        } else {
            Ok(())
        }
    }
    fn phase(&self, phase: &str, file: &str, done: u64, total: u64) {
        self.ctl.update_activity(|a| {
            a.phase = phase.into();
            a.file = file.into();
            a.done = done;
            a.total = total;
        });
    }
}

/// Aborts the task when dropped (a plain JoinHandle would let it run on).
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn spawn_watcher(
    ctl: Arc<Control>,
    cfg: Config,
    r: Arc<Remote>,
    name: String,
    stop: Arc<AtomicU8>,
    job: Option<JobInfo>,
) -> AbortOnDrop<()> {
    AbortOnDrop(tokio::spawn(async move {
        let (before, after) = (cfg.margin_before_min * 60, cfg.margin_after_min * 60);
        let block_at = job.as_ref().map(|j| j.next_block(now(), before, after));
        let forced = ctl.force_vm.lock().unwrap().as_deref() == Some(name.as_str());
        let mut tick = 0u64;
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            tick += 1;
            if ctl.cancel.load(Ordering::SeqCst) || (!ctl.running.load(Ordering::SeqCst) && !forced)
            {
                stop.store(STOP_USER, Ordering::SeqCst);
            }
            if let Some(b) = block_at
                && now() >= b
            {
                stop.store(STOP_WINDOW, Ordering::SeqCst);
            }
            if cfg.hours_limit
                && !forced
                && !schedule::hours_allowed(now(), cfg.hours_start, cfg.hours_end)
            {
                stop.store(STOP_HOURS, Ordering::SeqCst);
            }
            if tick.is_multiple_of(12)
                && let Ok(b) = r.busy(&cfg.remote_root).await
                && b.contains(&name)
            {
                stop.store(STOP_BUSY, Ordering::SeqCst);
            }
            if stop.load(Ordering::SeqCst) != STOP_NONE {
                ctl.repaint();
                return;
            }
        }
    }))
}

async fn get_dir(r: &Remote, cfg: &Config, name: &str) -> Result<RemoteDir> {
    let l = r.list(&cfg.remote_root).await?;
    l.dirs
        .into_iter()
        .find(|d| d.name == name)
        .ok_or_else(|| anyhow!("dizin sunucuda yok"))
}

async fn sync_vm(ctl: &Arc<Control>, cfg: &Config, r: Arc<Remote>, name: &str) -> Result<Outcome> {
    let stop = Arc::new(AtomicU8::new(STOP_NONE));
    let d0 = get_dir(&r, cfg, name).await?;
    let _w = spawn_watcher(
        ctl.clone(),
        cfg.clone(),
        r.clone(),
        name.to_string(),
        stop.clone(),
        schedule::estimate(&d0.files),
    );
    let env = Env {
        ctl,
        cfg,
        r,
        stop: stop.clone(),
        name,
    };
    match sync_passes(&env).await {
        Err(e) if e.is::<Cancelled>() => Ok(Outcome::Interrupted(
            stop_reason(stop.load(Ordering::SeqCst)).into(),
            None,
        )),
        other => other,
    }
}

struct Paths {
    root: PathBuf,
    live: PathBuf,
    staging: PathBuf,
    stblk: PathBuf,
    manifest: PathBuf,
    comblk: PathBuf,
    rhash: PathBuf,
}

fn paths(cfg: &Config, name: &str) -> Paths {
    let root = PathBuf::from(&cfg.local_root);
    let meta = cfg.meta_dir();
    Paths {
        live: root.join(name),
        staging: meta.join("staging").join(name),
        stblk: meta.join("staging-blk").join(name),
        manifest: meta
            .join("staging-blk")
            .join(format!("{name}.manifest.json")),
        comblk: meta.join("blk").join(name),
        rhash: meta.join("rhash").join(name),
        root,
    }
}

fn blk_name(file: &str) -> String {
    format!("{file}.blk")
}

/// Local file the delta for `f` is computed against (never for small files).
/// .vbk: same name if present, otherwise the live .vbk the server has renamed since (largest).
fn base_for(
    live: &[LocalFile],
    live_dir: &Path,
    remote: &[RemoteFile],
    f: &RemoteFile,
) -> Option<PathBuf> {
    if f.size < DELTA_MIN {
        return None;
    }
    if live.iter().any(|l| l.name == f.name) {
        return Some(live_dir.join(&f.name));
    }
    if !is_vbk(&f.name) {
        return None;
    }
    live.iter()
        .filter(|l| is_vbk(&l.name) && !remote.iter().any(|r| r.name == l.name))
        .max_by_key(|l| l.size)
        .map(|l| live_dir.join(&l.name))
}

/// True when the staging copy is a resumable download of exactly this remote version.
fn resumable(tgt: &Path, side_p: &Path, f: &RemoteFile) -> bool {
    load_blk_for(tgt, side_p).is_some_and(|s| s.remote_size == f.size && s.remote_mtime == f.mtime)
}

/// Remote block hashes, cached in `rh_p` and resumed from a partial cache after an interruption.
async fn remote_hashes(
    ctl: &Control,
    r: &Remote,
    rpath: &str,
    f: &RemoteFile,
    rh_p: &Path,
    mut progress: impl FnMut(u64) + Send,
    cancel: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<BlockHash>> {
    let total = block_count(f.size, BLOCK);
    let mut b = Blk::load(rh_p)
        .ok()
        .filter(|b| b.block == BLOCK && b.remote_size == f.size && b.remote_mtime == f.mtime)
        .unwrap_or_else(|| Blk::new(f.size, f.mtime));
    let mut k = b.hashes.iter().take_while(|h| **h != ZERO).count();
    if k == total {
        return Ok(b.hashes);
    }
    for h in &mut b.hashes[k..] {
        *h = ZERO;
    }
    if k > 0 {
        ctl.log(i18n::choose(
            format!(
                "{}: sunucu hash'i kaldığı yerden devam ediyor (%{:.0} hazır)",
                f.name,
                k as f64 * 100.0 / total as f64
            ),
            format!(
                "{}: resuming server hash ({:.0}% ready)",
                f.name,
                k as f64 * 100.0 / total as f64
            ),
        ));
    }
    progress(k as u64 * BLOCK);
    let mut last_save = Instant::now();
    let res = r
        .hash(
            rpath,
            BLOCK,
            k as u64,
            |hs| {
                for h in hs {
                    if k >= total {
                        bail!(
                            "{}",
                            i18n::choose(
                                format!(
                                    "{}: sunucu fazla hash döndürdü (dosya değişiyor olabilir)",
                                    f.name
                                ),
                                format!(
                                    "{}: server returned extra hashes (file may be changing)",
                                    f.name
                                )
                            )
                        );
                    }
                    b.hashes[k] = *h;
                    k += 1;
                }
                progress((k as u64 * BLOCK).min(f.size));
                if last_save.elapsed() > Duration::from_secs(30) {
                    b.save(rh_p)?;
                    last_save = Instant::now();
                }
                Ok(())
            },
            cancel,
        )
        .await;
    // keep the finished part even when interrupted: next time hashing continues from here
    let _ = b.save(rh_p);
    res?;
    if k != total {
        bail!(
            "{}",
            i18n::choose(
                format!(
                    "{}: hash sayısı uyuşmuyor ({k}/{total}, dosya değişiyor olabilir)",
                    f.name
                ),
                format!(
                    "{}: hash count mismatch ({k}/{total}, file may be changing)",
                    f.name
                )
            )
        );
    }
    Ok(b.hashes)
}

type HashTask = AbortOnDrop<Result<Vec<BlockHash>>>;

/// Hash a remote file in the background (server disk) while other files are downloading.
fn spawn_hash(env: &Env<'_>, rpath: String, f: RemoteFile, rh_p: PathBuf) -> HashTask {
    let (r, ctl, stop) = (env.r.clone(), env.ctl.clone(), env.stop.clone());
    AbortOnDrop(tokio::spawn(async move {
        let cancel = move || stop.load(Ordering::SeqCst) != STOP_NONE;
        let mut last = Instant::now() - Duration::from_secs(5);
        let res = remote_hashes(
            &ctl,
            &r,
            &rpath,
            &f,
            &rh_p,
            |done| {
                if last.elapsed() > Duration::from_secs(1) {
                    last = Instant::now();
                    let pctv = done as f64 * 100.0 / f.size.max(1) as f64;
                    ctl.update_activity(|a| {
                        a.bg = i18n::choose(
                            format!("Arka planda sunucu hash'i: {} %{pctv:.0}", f.name),
                            format!("Background server hash: {} {pctv:.0}%", f.name),
                        )
                    });
                }
            },
            &cancel,
        )
        .await;
        ctl.update_activity(|a| a.bg.clear());
        res
    }))
}

async fn sync_passes(env: &Env<'_>) -> Result<Outcome> {
    let (ctl, cfg, name) = (env.ctl, env.cfg, env.name);
    let p = paths(cfg, name);
    for d in [&p.staging, &p.stblk, &p.comblk, &p.rhash] {
        fs::create_dir_all(d).with_context(|| {
            i18n::choose(
                format!("dizin oluşturulamadı: {}", d.display()),
                format!("could not create folder: {}", d.display()),
            )
        })?;
    }
    for pass in 1..=4 {
        env.check()?;
        let d = get_dir(&env.r, cfg, name).await?;
        if d.busy {
            return Ok(Outcome::Interrupted(
                i18n::text("sunucuda job çalışıyor").into(),
                None,
            ));
        }
        let mut files = d.files.clone();
        files.sort_by_key(|f| (file_order(&f.name), f.name.clone()));
        let l0 = recs(&d.files);
        let mut manifest: BTreeMap<String, FileRec> = fs::read(&p.manifest)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();

        // --- clean staging: keep partial .vbk by renaming it to the new .vbk name
        let new_vbk = files
            .iter()
            .find(|f| is_vbk(&f.name))
            .map(|f| f.name.clone());
        for lf in list_local_files(&p.staging) {
            if files.iter().any(|f| f.name == lf.name) {
                continue;
            }
            if is_vbk(&lf.name)
                && let Some(nv) = &new_vbk
                && !p.staging.join(nv).exists()
            {
                fs::rename(p.staging.join(&lf.name), p.staging.join(nv))?;
                let _ = fs::rename(p.stblk.join(blk_name(&lf.name)), p.stblk.join(blk_name(nv)));
                ctl.log(i18n::choose(
                    format!("{name}: yarım {} → {} olarak devam", lf.name, nv),
                    format!("{name}: resuming partial {} → {}", lf.name, nv),
                ));
                continue;
            }
            fs::remove_file(p.staging.join(&lf.name))?;
            let _ = fs::remove_file(p.stblk.join(blk_name(&lf.name)));
        }
        manifest.retain(|n, rec| l0.contains(rec) && p.staging.join(n).exists());

        let st = ctl
            .state
            .lock()
            .unwrap()
            .vms
            .get(name)
            .cloned()
            .unwrap_or_default();
        let committed = st.committed.clone().unwrap_or_default();
        let linkable = |f: &RemoteFile| {
            committed.iter().any(|c| same_rec(c, f))
                && fs::metadata(p.live.join(&f.name))
                    .map(|m| m.len() == f.size)
                    .unwrap_or(false)
        };
        let live_files = list_local_files(&p.live);

        // --- plan: what is unchanged, what needs work, how much disk it takes
        let mut need = 0u64;
        let (mut n_link, mut n_work, mut work_bytes) = (0usize, 0usize, 0u64);
        for f in &files {
            if manifest.contains_key(&f.name) || linkable(f) {
                n_link += 1;
                continue;
            }
            n_work += 1;
            work_bytes += f.size;
            let tgt = p.staging.join(&f.name);
            let base = base_for(&live_files, &p.live, &files, f);
            need += if let Ok(m) = fs::metadata(&tgt) {
                f.size.saturating_sub(m.len())
            } else if cfg.mode == UpdateMode::InPlace
                && is_vbk(&f.name)
                && let Some(b) = &base
            {
                f.size
                    .saturating_sub(fs::metadata(b).map(|m| m.len()).unwrap_or(0))
            } else {
                f.size
            };
        }
        let reserve = cfg.min_free_gb << 30;
        let mut free = fs2::available_space(&p.root).unwrap_or(0);
        if free < need + reserve && cfg.free_space_first {
            delete_legacy(ctl, cfg, name);
            free = fs2::available_space(&p.root).unwrap_or(0);
        }
        if free < need + reserve {
            bail!(
                "{}",
                i18n::choose(
                    format!(
                        "yetersiz disk alanı: gereken {} (+{} rezerv), boş {}",
                        human(need),
                        human(reserve),
                        human(free)
                    ),
                    format!(
                        "insufficient disk space: need {} (+{} reserve), free {}",
                        human(need),
                        human(reserve),
                        human(free)
                    )
                )
            );
        }
        if pass == 1 {
            ctl.log(i18n::choose(format!(
                "{name}: {} dosya — {n_link} değişmemiş (kopyalanmaz), {n_work} yeni/değişmiş ({}), ek disk ~{}",
                files.len(),
                human(work_bytes),
                human(need)
            ), format!(
                "{name}: {} files — {n_link} unchanged (no copy), {n_work} new/changed ({}), additional disk ~{}",
                files.len(),
                human(work_bytes),
                human(need)
            )));
        }

        // --- start server-side hashing of large changed files right away, so it runs while the
        //     .vib files are downloading instead of afterwards
        let mut pre: HashMap<String, HashTask> = HashMap::new();
        for f in files.iter().filter(|f| f.size >= DELTA_MIN) {
            if manifest.contains_key(&f.name) || linkable(f) {
                continue;
            }
            let tgt = p.staging.join(&f.name);
            let side_p = p.stblk.join(blk_name(&f.name));
            let rh_p = p.rhash.join(blk_name(&f.name));
            let cached = Blk::load(&rh_p).is_ok_and(|b| {
                b.remote_size == f.size && b.remote_mtime == f.mtime && b.complete()
            });
            let has_local = tgt.exists() || base_for(&live_files, &p.live, &files, f).is_some();
            if !cached && has_local && !resumable(&tgt, &side_p, f) {
                let rpath = format!(
                    "{}/{}/{}",
                    cfg.remote_root.trim_end_matches('/'),
                    name,
                    f.name
                );
                pre.insert(f.name.clone(), spawn_hash(env, rpath, f.clone(), rh_p));
            }
        }

        // --- transfer
        let mut stable = true;
        for f in &files {
            env.check()?;
            if manifest.contains_key(&f.name) {
                continue;
            }
            let tgt = p.staging.join(&f.name);
            let side_p = p.stblk.join(blk_name(&f.name));
            if linkable(f) {
                if tgt.exists() {
                    fs::remove_file(&tgt)?;
                }
                match fs::hard_link(p.live.join(&f.name), &tgt) {
                    Ok(()) => {
                        let cb = p.comblk.join(blk_name(&f.name));
                        if cb.exists() {
                            let _ = fs::copy(&cb, &side_p);
                        }
                        manifest.insert(
                            f.name.clone(),
                            FileRec {
                                name: f.name.clone(),
                                size: f.size,
                                mtime: f.mtime,
                            },
                        );
                        save_manifest(&p.manifest, &manifest)?;
                        continue;
                    }
                    Err(e) => ctl.log(i18n::choose(
                        format!("{name}: hardlink olmadı ({e}), indirilecek: {}", f.name),
                        format!("{name}: hard link failed ({e}), will download: {}", f.name),
                    )),
                }
            }
            // the in-place move may have consumed the base already
            let base = base_for(&live_files, &p.live, &files, f).filter(|b| b.exists());
            let ok = transfer(env, &p, f, &tgt, &side_p, base, pre.remove(&f.name)).await?;
            filetime::set_file_mtime(&tgt, filetime::FileTime::from_unix_time(f.mtime, 0))?;
            if ok {
                manifest.insert(
                    f.name.clone(),
                    FileRec {
                        name: f.name.clone(),
                        size: f.size,
                        mtime: f.mtime,
                    },
                );
                save_manifest(&p.manifest, &manifest)?;
            } else {
                stable = false;
            }
        }
        drop(pre);

        env.check()?;
        let d1 = get_dir(&env.r, cfg, name).await?;
        if stable
            && recs(&d1.files) == l0
            && files.iter().all(|f| manifest.contains_key(&f.name))
            && !d1.busy
        {
            commit(env, &p, &l0)?;
            return Ok(Outcome::Committed);
        }
        if d1.busy {
            return Ok(Outcome::Interrupted(
                i18n::text("sunucuda job başladı").into(),
                None,
            ));
        }
        ctl.log(i18n::choose(
            format!("{name}: sunucuda dosyalar değişti, {}. geçiş", pass + 1),
            format!("{name}: server files changed, pass {}", pass + 1),
        ));
    }
    Ok(Outcome::Interrupted(
        i18n::text("dosyalar sürekli değişiyor, 15 dk sonra tekrar denenecek").into(),
        Some(900),
    ))
}

fn save_manifest(p: &Path, m: &BTreeMap<String, FileRec>) -> Result<()> {
    crate::config::write_atomic(p, &serde_json::to_vec(m)?)
}

fn commit(env: &Env<'_>, p: &Paths, l0: &[FileRec]) -> Result<()> {
    let (ctl, cfg, name) = (env.ctl, env.cfg, env.name);
    env.phase(i18n::text("Tamamlanıyor"), "", 0, 0);
    let mut trash = None;
    if p.live.exists() {
        let t = cfg
            .meta_dir()
            .join("trash")
            .join(format!("{name}-{}", now()));
        fs::create_dir_all(t.parent().unwrap())?;
        fs::rename(&p.live, &t).with_context(|| {
            i18n::choose(
                format!("{} taşınamadı (açık dosya olabilir)", p.live.display()),
                format!("could not move {} (file may be open)", p.live.display()),
            )
        })?;
        trash = Some(t);
    }
    if let Err(e) = fs::rename(&p.staging, &p.live) {
        if let Some(t) = &trash {
            let _ = fs::rename(t, &p.live);
        }
        return Err(anyhow!(e).context(i18n::text("yeni set yerine taşınamadı")));
    }
    // sidecars of the new set
    if p.comblk.exists() {
        fs::remove_dir_all(&p.comblk)?;
    }
    fs::rename(&p.stblk, &p.comblk)?;
    let _ = fs::remove_file(&p.manifest);
    {
        let mut st = ctl.state.lock().unwrap();
        let v = st.vms.entry(name.to_string()).or_default();
        v.committed = Some(l0.to_vec());
        v.committed_at = Some(now());
        v.live_set_incomplete = false;
        v.last_error = None;
    }
    ctl.save_state();
    // only now: the old set and the remote hash cache are no longer needed
    if let Some(t) = trash
        && let Err(e) = fs::remove_dir_all(&t)
    {
        ctl.log(i18n::choose(
            format!(
                "{name}: eski set silinemedi {}: {e} (sonraki açılışta tekrar denenecek)",
                t.display()
            ),
            format!(
                "{name}: could not delete old set {}: {e} (retry on next launch)",
                t.display()
            ),
        ));
    }
    let _ = fs::remove_dir_all(&p.rhash);
    delete_legacy(ctl, cfg, name);
    Ok(())
}

/// Delete the approved old local dirs of this VM (e.g. 101_DATABASE_NODE01).
fn delete_legacy(ctl: &Control, cfg: &Config, name: &str) {
    let root = PathBuf::from(&cfg.local_root);
    let Some(list) = cfg.legacy_approved.get(name) else {
        return;
    };
    for d in list {
        if d.is_empty()
            || d == name
            || d.starts_with('.')
            || d.contains(['\\', '/', ':'])
            || vm_key(d) != vm_key(name)
        {
            continue;
        }
        let pth = root.join(d);
        if !pth.is_dir() {
            continue;
        }
        let sz = dir_size(&pth);
        match fs::remove_dir_all(&pth) {
            Ok(()) => ctl.log(i18n::choose(
                format!("🗑 {name}: eski yerel dizin silindi: {d} ({})", human(sz)),
                format!("🗑 {name}: deleted legacy local folder: {d} ({})", human(sz)),
            )),
            Err(e) => ctl.log(format!("{name}: {d} silinemedi: {e}")),
        }
    }
}

// ---------------------------------------------------------------- file transfer

fn load_blk_for(path: &Path, side: &Path) -> Option<Blk> {
    let len = fs::metadata(path).ok()?.len();
    Blk::load(side)
        .ok()
        .filter(|b| b.remote_size == len && b.block == BLOCK)
}

fn hash_local(env: &Env<'_>, path: &Path, label: &str) -> Result<Blk> {
    let len = fs::metadata(path)?.len();
    let mut b = Blk::new(len, i64::MIN);
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; BLOCK as usize];
    let fname = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    for i in 0..b.hashes.len() {
        env.check()?;
        let n = b.block_len(i) as usize;
        f.read_exact(&mut buf[..n])?;
        b.hashes[i] = hash_block(&buf[..n]);
        if i % 16 == 0 {
            env.phase(
                &i18n::choose(
                    format!("Yerel hash ({label})"),
                    format!("Local hash ({label})"),
                ),
                &fname,
                i as u64 * BLOCK,
                len,
            );
        }
    }
    Ok(b)
}

/// Returns Ok(true) when the target is a complete, verified copy of the remote version.
async fn transfer(
    env: &Env<'_>,
    p: &Paths,
    f: &RemoteFile,
    tgt: &Path,
    side_p: &Path,
    mut base: Option<PathBuf>,
    pre: Option<HashTask>,
) -> Result<bool> {
    let (ctl, cfg, name) = (env.ctl, env.cfg, env.name);
    let rpath = format!(
        "{}/{}/{}",
        cfg.remote_root.trim_end_matches('/'),
        name,
        f.name
    );

    // Never write through a hard link (it would modify the live copy as well).
    if tgt.exists() && link_count(tgt)? > 1 {
        fs::remove_file(tgt)?;
        let _ = fs::remove_file(side_p);
    }

    // In-place mode: move the live .vbk into staging and patch it.
    if cfg.mode == UpdateMode::InPlace
        && is_vbk(&f.name)
        && !tgt.exists()
        && let Some(b) = base.take()
    {
        let bname = b
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        {
            let mut st = ctl.state.lock().unwrap();
            st.vms
                .entry(name.to_string())
                .or_default()
                .live_set_incomplete = true;
        }
        ctl.save_state();
        fs::rename(&b, tgt).with_context(|| {
            i18n::choose(
                format!("{} taşınamadı", b.display()),
                format!("could not move {}", b.display()),
            )
        })?;
        let cb = p.comblk.join(blk_name(&bname));
        if cb.exists() {
            let _ = fs::copy(&cb, side_p);
        }
        ctl.log(i18n::choose(
            format!("{name}: {bname} yerinde güncellenecek → {}", f.name),
            format!("{name}: updating {bname} in place → {}", f.name),
        ));
    }

    let mut side: Option<Blk> = if tgt.exists() {
        load_blk_for(tgt, side_p)
    } else {
        None
    };
    if tgt.exists() && side.is_none() {
        let len = fs::metadata(tgt)?.len();
        if len >= DELTA_MIN {
            // rare: sidecar lost (crash). Re-hashing locally is cheaper than downloading again.
            side = Some(hash_local(env, tgt, "hedef")?);
            side.as_ref().unwrap().save(side_p)?;
        } else {
            fs::remove_file(tgt)?;
        }
    }

    let rh_p = p.rhash.join(blk_name(&f.name));
    let mut hr: Option<Vec<BlockHash>> = Blk::load(&rh_p)
        .ok()
        .filter(|b| b.remote_size == f.size && b.remote_mtime == f.mtime && b.complete())
        .map(|b| b.hashes);

    let resume = side
        .as_ref()
        .is_some_and(|s| s.remote_size == f.size && s.remote_mtime == f.mtime);
    let has_local = side.as_ref().is_some_and(|s| s.valid_count() > 0) || base.is_some();
    let mut side = if resume {
        side.unwrap()
    } else if !has_local || f.size < DELTA_MIN {
        Blk::new(f.size, f.mtime)
    } else {
        // delta: need remote hashes
        if hr.is_none() {
            let h = match pre {
                Some(mut task) => {
                    env.phase(i18n::text("Sunucu hash'i bekleniyor"), &f.name, 0, 0);
                    match (&mut task.0).await {
                        Ok(r) => r?,
                        Err(e) => bail!(
                            "{}",
                            i18n::choose(format!("hash görevi: {e}"), format!("hash task: {e}"))
                        ),
                    }
                }
                None => {
                    env.phase(i18n::text("Sunucuda hash hesaplanıyor"), &f.name, 0, f.size);
                    let mut last = Instant::now();
                    remote_hashes(
                        ctl,
                        &env.r,
                        &rpath,
                        f,
                        &rh_p,
                        |done| {
                            if last.elapsed() > Duration::from_millis(500) {
                                last = Instant::now();
                                ctl.update_activity(|a| {
                                    a.phase = i18n::text("Sunucuda hash hesaplanıyor").into();
                                    a.done = done;
                                    a.total = f.size;
                                });
                            }
                        },
                        &|| env.stopped(),
                    )
                    .await?
                }
            };
            if h.len() != block_count(f.size, BLOCK) {
                bail!(
                    "{}",
                    i18n::choose(
                        format!(
                            "{}: hash sayısı uyuşmuyor (dosya değişiyor olabilir)",
                            f.name
                        ),
                        format!("{}: hash count mismatch (file may be changing)", f.name)
                    )
                );
            }
            hr = Some(h);
        }
        let hrv = hr.as_ref().unwrap();
        let mut s = side.take().unwrap_or_else(|| Blk::new(f.size, f.mtime));
        s.retarget(f.size, f.mtime);
        for (i, h) in s.hashes.iter_mut().enumerate() {
            if *h != hrv[i] {
                *h = ZERO;
            }
        }
        s
    };

    // open target and persist the (possibly invalidated) sidecar before writing anything
    let out = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(tgt)?;
    out.set_len(f.size)?;
    side.save(side_p)?;

    let need: Vec<usize> = (0..side.hashes.len())
        .filter(|i| side.hashes[*i] == ZERO)
        .collect();
    let total_need: u64 = need.iter().map(|i| side.block_len(*i)).sum();
    let mut done_bytes = 0u64;

    // --- copy matching blocks from the local base (Safe mode / leftovers)
    let mut fetch: Vec<usize> = need.clone();
    if let (Some(b), Some(hrv)) = (&base, &hr)
        && !need.is_empty()
    {
        let bname = b
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let bblk = match load_blk_for(b, &p.comblk.join(blk_name(&bname))) {
            Some(x) => x,
            None => hash_local(env, b, "kaynak")?,
        };
        let bf = File::open(b)?;
        let mut buf = vec![0u8; BLOCK as usize];
        let mut since = 0;
        let mut rest = Vec::new();
        for &i in &need {
            env.check()?;
            if i < bblk.hashes.len()
                && bblk.hashes[i] == hrv[i]
                && bblk.block_len(i) == side.block_len(i)
            {
                let n = side.block_len(i) as usize;
                let off = i as u64 * BLOCK;
                bf.seek_read_exact(&mut buf[..n], off)?;
                let h = hash_block(&buf[..n]);
                if h == hrv[i] {
                    out.seek_write_all(&buf[..n], off)?;
                    side.hashes[i] = h;
                    done_bytes += n as u64;
                    since += 1;
                    if since >= BATCH {
                        out.sync_data()?;
                        side.save(side_p)?;
                        since = 0;
                        env.phase(
                            i18n::text("Yerel bloklar kopyalanıyor"),
                            &f.name,
                            done_bytes,
                            total_need,
                        );
                    }
                    continue;
                }
            }
            rest.push(i);
        }
        out.sync_data()?;
        side.save(side_p)?;
        fetch = rest;
    }

    // --- fetch remaining blocks from the server
    let mut unstable = false;
    if !fetch.is_empty() {
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for &i in &fetch {
            let off = i as u64 * BLOCK;
            let len = side.block_len(i);
            match ranges.last_mut() {
                Some((o, l)) if *o + *l == off => *l += len,
                _ => ranges.push((off, len)),
            }
        }
        let fetch_bytes: u64 = ranges.iter().map(|r| r.1).sum();
        let unchanged = f.size.saturating_sub(fetch_bytes);
        ctl.log(format!(
            "{name}: {} → {} indirilecek{}",
            f.name,
            human(fetch_bytes),
            if resume {
                i18n::text(" (kaldığı yerden)").to_string()
            } else if hr.is_some() {
                i18n::choose(
                    format!(" (delta: {} aynı, atlandı)", human(unchanged)),
                    format!(" (delta: {} unchanged, skipped)", human(unchanged)),
                )
            } else {
                String::new()
            }
        ));
        let mut idx = 0usize;
        let mut buf: Vec<u8> = Vec::with_capacity(BLOCK as usize);
        let mut since = 0usize;
        let t0 = Instant::now();
        let mut got: u64 = 0;
        let mut last_ui = Instant::now();
        let mut win_t = Instant::now();
        let mut win_b = 0u64;
        let mut speed = *ctl.speed_est.lock().unwrap();
        let limit = cfg.max_mbps * 1e6 / 8.0;
        let side_ref = &mut side;
        let fname = f.name.clone();
        env.phase(i18n::text("İndiriliyor"), &f.name, done_bytes, total_need);
        let res = env
            .r
            .read_ranges(
                &rpath,
                &ranges,
                |mut data: &[u8]| {
                    got += data.len() as u64;
                    ctl.net_total
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                    win_b += data.len() as u64;
                    while !data.is_empty() {
                        if idx >= fetch.len() {
                            bail!(
                                "{}",
                                i18n::choose(
                                    format!("{}: sunucu beklenenden fazla veri gönderdi", fname),
                                    format!("{}: server sent more data than expected", fname)
                                )
                            );
                        }
                        let i = fetch[idx];
                        let n = side_ref.block_len(i) as usize;
                        let take = (n - buf.len()).min(data.len());
                        buf.extend_from_slice(&data[..take]);
                        data = &data[take..];
                        if buf.len() == n {
                            let h = hash_block(&buf);
                            let off = i as u64 * BLOCK;
                            out.seek_write_all(&buf, off)?;
                            match &hr {
                                Some(hrv) if hrv[i] != h => unstable = true,
                                _ => side_ref.hashes[i] = h,
                            }
                            buf.clear();
                            idx += 1;
                            since += 1;
                            if since >= BATCH {
                                out.sync_data()?;
                                side_ref.save(side_p)?;
                                since = 0;
                            }
                        }
                    }
                    if limit > 0.0 {
                        let should = got as f64 / limit;
                        let el = t0.elapsed().as_secs_f64();
                        if should > el {
                            std::thread::sleep(Duration::from_secs_f64((should - el).min(2.0)));
                        }
                    }
                    if last_ui.elapsed() > Duration::from_millis(400) {
                        let dt = win_t.elapsed().as_secs_f64();
                        if dt > 2.0 {
                            let s = win_b as f64 / dt;
                            speed = if speed > 0.0 {
                                speed * 0.7 + s * 0.3
                            } else {
                                s
                            };
                            win_t = Instant::now();
                            win_b = 0;
                        }
                        last_ui = Instant::now();
                        let done = done_bytes + got;
                        ctl.update_activity(|a| {
                            a.phase = i18n::text("İndiriliyor").into();
                            a.file = fname.clone();
                            a.done = done;
                            a.total = total_need;
                            a.speed = speed;
                        });
                    }
                    Ok(())
                },
                &|| env.stopped(),
            )
            .await;
        // always checkpoint what we have
        let _ = out.sync_data();
        let _ = side.save(side_p);
        ctl.update_activity(|a| a.net_bytes += got);
        if t0.elapsed().as_secs() > 30 && got > 100_000_000 {
            *ctl.speed_est.lock().unwrap() = got as f64 / t0.elapsed().as_secs_f64();
        }
        res?;
        if idx != fetch.len() {
            bail!(
                "{}",
                i18n::choose(
                    format!(
                        "{}: veri eksik geldi ({} / {} blok)",
                        f.name,
                        idx,
                        fetch.len()
                    ),
                    format!(
                        "{}: incomplete data ({} / {} blocks)",
                        f.name,
                        idx,
                        fetch.len()
                    )
                )
            );
        }
    } else {
        ctl.log(i18n::choose(
            format!(
                "{name}: {} → indirme gerekmedi (yerel bloklar yeterli)",
                f.name
            ),
            format!(
                "{name}: {} → no download needed (local blocks sufficient)",
                f.name
            ),
        ));
    }
    out.sync_all()?;
    drop(out);
    side.save(side_p)?;
    if unstable {
        ctl.log(i18n::choose(
            format!(
                "{name}: {} indirilirken sunucuda değişti, tekrar kontrol edilecek",
                f.name
            ),
            format!(
                "{name}: {} changed on the server during download; will recheck",
                f.name
            ),
        ));
    }
    Ok(side.complete() && !unstable)
}

fn link_count(p: &Path) -> Result<u32> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let f = File::open(p)?;
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(f.as_raw_handle() as _, &mut info) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(info.nNumberOfLinks)
    }
}

trait PosIo {
    fn seek_read_exact(&self, buf: &mut [u8], off: u64) -> std::io::Result<()>;
    fn seek_write_all(&self, buf: &[u8], off: u64) -> std::io::Result<()>;
}

impl PosIo for File {
    fn seek_read_exact(&self, mut buf: &mut [u8], mut off: u64) -> std::io::Result<()> {
        while !buf.is_empty() {
            let n = self.seek_read(buf, off)?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            buf = &mut buf[n..];
            off += n as u64;
        }
        Ok(())
    }
    fn seek_write_all(&self, mut buf: &[u8], mut off: u64) -> std::io::Result<()> {
        while !buf.is_empty() {
            let n = self.seek_write(buf, off)?;
            if n == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            buf = &buf[n..];
            off += n as u64;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn shutdown_before_start_acknowledges_without_network_or_recovery() {
        let ctl = Control::new(Config::default());
        ctl.shutdown.store(true, Ordering::SeqCst);
        engine(ctl.clone()).await;
        assert!(ctl.engine_stopped.load(Ordering::SeqCst));
        let shared = ctl.shared.lock().unwrap();
        assert!(shared.log.is_empty());
        assert!(shared.activity.is_none());
    }
    #[test]
    fn deleted_or_truncated_local_backup_is_pending() {
        let root = std::env::temp_dir().join(format!("hbsync-local-proof-{}", std::process::id()));
        fs::create_dir_all(root.join("VM")).unwrap();
        let cfg = Config {
            auto_start: true,
            local_root: root.to_string_lossy().into(),
            ..Config::default()
        };
        let ctl = Control::new(cfg.clone());
        let listing = RemoteListing {
            dirs: vec![RemoteDir {
                name: "VM".into(),
                busy: false,
                files: vec![rf("backup.vbk", 4)],
            }],
            disk_total: 100,
            disk_free: 50,
        };
        ctl.state
            .lock()
            .unwrap()
            .vms
            .entry("VM".into())
            .or_default()
            .committed = Some(recs(&listing.dirs[0].files));
        fs::write(root.join("VM/backup.vbk"), b"good").unwrap();
        refresh_views(&ctl, &cfg, &listing);
        assert!(
            ctl.shared.lock().unwrap().vms[0].in_sync,
            "unaffected control: intact set"
        );
        fs::remove_file(root.join("VM/backup.vbk")).unwrap();
        refresh_views(&ctl, &cfg, &listing);
        let actual = ctl.shared.lock().unwrap().vms[0].in_sync;
        println!("EXPECTED: missing backup is not current; ACTUAL: in_sync={actual}");
        assert!(
            !actual,
            "PROBLEM CONFIRMED: missing local backup reported current"
        );
        assert!(
            matches!(pick(&ctl, &cfg, &listing), Pick::Vm(_)),
            "missing backup must be scheduled for repair"
        );
        fs::write(root.join("VM/backup.vbk"), b"x").unwrap();
        refresh_views(&ctl, &cfg, &listing);
        assert!(!ctl.shared.lock().unwrap().vms[0].in_sync);
        assert!(ctl.shared.lock().unwrap().vms[0].est_bytes > 0);
        fs::remove_dir_all(root).unwrap();
        println!("PROBLEM NOT REPRODUCED; FIX VERIFIED");
    }
    #[test]
    fn keys() {
        assert_eq!(vm_key("101_DATABASE_HELSINKI"), "101_DATABASE");
        assert_eq!(vm_key("101_DATABASE_NODE01"), "101_DATABASE");
        assert_eq!(vm_key("74_VMMANAGER__HELSINKI"), "74_VMMANAGER");
        assert_eq!(vm_key("74_VMMANAGER"), "74_VMMANAGER");
        assert_eq!(vm_key("95_DALGAKIRAN_NODE01_1"), "95_DALGAKIRAN");
        assert_eq!(vm_key("99_POSTAL_1"), "99_POSTAL_1");
        assert_eq!(vm_key("CLIENT_SHOP_NODE01"), "CLIENT_SHOP");
    }

    fn rf(name: &str, size: u64) -> RemoteFile {
        RemoteFile {
            name: name.into(),
            size,
            mtime: 0,
        }
    }
    fn lf(name: &str, size: u64) -> LocalFile {
        LocalFile {
            name: name.into(),
            size,
        }
    }

    #[test]
    fn base_selection() {
        let dir = Path::new("L");
        let big = 10 << 30;
        let remote = vec![rf("A.vbk", big), rf("B.vib", big), rf("x.vbm", 1 << 20)];
        // renamed .vbk: the old one (not on the server any more) is the base
        let live = vec![lf("OLD.vbk", big), lf("x.vbm", 1 << 20)];
        assert_eq!(
            base_for(&live, dir, &remote, &remote[0]),
            Some(dir.join("OLD.vbk"))
        );
        // same name wins
        let live2 = vec![lf("A.vbk", big), lf("OLD.vbk", big)];
        assert_eq!(
            base_for(&live2, dir, &remote, &remote[0]),
            Some(dir.join("A.vbk"))
        );
        // a .vib never borrows another file; small files are never delta'd
        assert_eq!(base_for(&live, dir, &remote, &remote[1]), None);
        assert_eq!(base_for(&live, dir, &remote, &remote[2]), None);
        // a live .vbk still present on the server is not "spare"
        let remote2 = vec![rf("A.vbk", big), rf("OLD.vbk", big)];
        assert_eq!(
            base_for(&[lf("OLD.vbk", big)], dir, &remote2, &remote2[0]),
            None
        );
    }
}
