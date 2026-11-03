//! Persistent download statistics (all-time total, per-day totals).

use crate::config::{Config, write_atomic};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    #[serde(skip)]
    path: Option<PathBuf>,
    pub total_bytes: u64,
    /// seconds in which data was actually received
    pub active_secs: u64,
    /// first time anything was counted (epoch)
    pub since: i64,
    /// "YYYY-MM-DD" (local) -> bytes
    pub daily: BTreeMap<String, u64>,
}

impl Stats {
    fn path() -> PathBuf {
        Config::path().with_file_name("stats.json")
    }
    pub fn load_for(config: &Config) -> Stats {
        let path = config.save_path().with_file_name("stats.json");
        let mut stats: Stats = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        stats.path = Some(path);
        stats
    }
    pub fn save(&self) {
        if let Ok(b) = serde_json::to_vec_pretty(self) {
            let _ = write_atomic(&self.path.clone().unwrap_or_else(Self::path), &b);
        }
    }
    pub fn add(&mut self, bytes: u64) {
        if self.since == 0 {
            self.since = chrono::Utc::now().timestamp();
        }
        self.total_bytes += bytes;
        self.active_secs += 1;
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        *self.daily.entry(day).or_default() += bytes;
        while self.daily.len() > 120 {
            let k = self.daily.keys().next().cloned().unwrap();
            self.daily.remove(&k);
        }
    }
}
