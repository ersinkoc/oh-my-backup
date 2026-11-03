//! Estimate when the Veeam job writes into a backup dir, from file names + mtimes.
//!
//! Veeam names restore points like `VM.19D2026-10-29T063010_983F.vib` where the timestamp is the
//! job start in the Veeam server's local time. The file mtime (epoch) is the moment the job
//! finished writing it. From those pairs we derive the timezone offset, start time and duration.

use crate::config::Plan;
use crate::remote::RemoteFile;
use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike};
use regex::Regex;
use std::sync::OnceLock;

pub const DAY: i64 = 86_400;

#[derive(Debug, Clone)]
pub struct JobInfo {
    /// epoch of the most recent job start
    pub last_start: i64,
    /// epoch of the most recent job end (latest mtime in dir)
    pub last_end: i64,
    /// longest observed duration (seconds)
    pub max_dur: i64,
    pub samples: usize,
}

fn re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(\d{4})-(\d{2})-(\d{2})T(\d{2})(\d{2})(\d{2})_[0-9A-Fa-f]{4}\.(vib|vbk|vrb)$")
            .unwrap()
    })
}

/// naive timestamp from the name, interpreted as if it were UTC
pub fn name_time(name: &str) -> Option<i64> {
    let c = re().captures(name)?;
    let n = |i: usize| c[i].parse::<u32>().ok();
    let d = NaiveDate::from_ymd_opt(c[1].parse().ok()?, n(2)?, n(3)?)?;
    let t = d.and_hms_opt(n(4)?, n(5)?, n(6)?)?;
    Some(t.and_utc().timestamp())
}

pub fn estimate(files: &[RemoteFile]) -> Option<JobInfo> {
    let latest_mtime = files.iter().map(|f| f.mtime).max()?;
    // (naive, end) pairs. vib mtime == its own job end. The newest point ends at latest_mtime.
    let mut pts: Vec<(i64, i64, bool)> = files
        .iter()
        .filter_map(|f| {
            name_time(&f.name).map(|n| {
                (
                    n,
                    f.mtime,
                    f.name.ends_with(".vib") || f.name.ends_with(".vrb"),
                )
            })
        })
        .collect();
    if pts.is_empty() {
        return None;
    }
    pts.sort();
    let newest = pts.last().unwrap().0;
    let mut samples: Vec<(i64, i64)> = pts
        .iter()
        .filter(|p| p.2 && p.0 != newest)
        .map(|p| (p.0, p.1))
        .collect();
    samples.push((newest, latest_mtime));

    // timezone offset (hours) = ceil((naive - end)/3600), assuming job duration < 1h; take the mode
    let mut tz_votes: Vec<i64> = samples
        .iter()
        .map(|(n, e)| ((n - e) as f64 / 3600.0).ceil() as i64)
        .collect();
    tz_votes.sort();
    let tz = mode(&tz_votes);

    let mut max_dur = 60;
    let mut last_start = 0;
    for (n, e) in &samples {
        let start = n - tz * 3600;
        let dur = (e - start).clamp(60, 6 * 3600);
        max_dur = max_dur.max(dur);
        last_start = last_start.max(start);
    }
    Some(JobInfo {
        last_start,
        last_end: latest_mtime,
        max_dur,
        samples: samples.len(),
    })
}

fn mode(v: &[i64]) -> i64 {
    let mut best = (v[0], 0);
    let mut i = 0;
    while i < v.len() {
        let mut j = i;
        while j < v.len() && v[j] == v[i] {
            j += 1;
        }
        if j - i > best.1 {
            best = (v[i], j - i);
        }
        i = j;
    }
    best.0
}

impl JobInfo {
    /// Avoid windows [start, end] for days around `now`.
    fn windows(&self, now: i64, before: i64, after: i64) -> Vec<(i64, i64)> {
        let k0 = (now - self.last_start).div_euclid(DAY);
        let mut v: Vec<(i64, i64)> = (k0 - 1..=k0 + 2)
            .map(|k| {
                let s = self.last_start + k * DAY;
                (s - before, s + self.max_dur + after)
            })
            .collect();
        v.push((self.last_start - before, self.last_end + after));
        v
    }

    /// If `now` is inside an avoid window, returns when it ends.
    pub fn blocked_until(&self, now: i64, before: i64, after: i64) -> Option<i64> {
        self.windows(now, before, after)
            .into_iter()
            .filter(|(s, e)| *s <= now && now < *e)
            .map(|(_, e)| e)
            .max()
    }

    /// Start of the next avoid window after `now`.
    pub fn next_block(&self, now: i64, before: i64, after: i64) -> i64 {
        self.windows(now, before, after)
            .into_iter()
            .map(|(s, _)| s)
            .filter(|s| *s > now)
            .min()
            .unwrap_or(now + DAY)
    }

    /// Human readable daily window in local time, e.g. "06:30–06:33"
    pub fn describe(&self) -> String {
        format!(
            "{}–{}",
            hm(self.last_start),
            hm(self.last_start + self.max_dur)
        )
    }
}

pub fn hm(epoch: i64) -> String {
    Local
        .timestamp_opt(epoch, 0)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

pub fn fmt_time(epoch: i64) -> String {
    Local
        .timestamp_opt(epoch, 0)
        .single()
        .map(|t| t.format("%d.%m %H:%M").to_string())
        .unwrap_or_default()
}

/// Global allowed-hours check (local time). start==end means always.
pub fn hours_allowed(now: i64, start: u32, end: u32) -> bool {
    if start == end {
        return true;
    }
    let h = Local
        .timestamp_opt(now, 0)
        .single()
        .map(|t| t.hour())
        .unwrap_or(0);
    if start < end {
        h >= start && h < end
    } else {
        h >= start || h < end
    }
}

/// Next epoch at which the allowed-hours window opens.
pub fn hours_next_open(now: i64, start: u32) -> i64 {
    let mut t = now - now.rem_euclid(3600) + 3600;
    for _ in 0..48 {
        if Local.timestamp_opt(t, 0).single().map(|x| x.hour()) == Some(start) {
            return t;
        }
        t += 3600;
    }
    now + 3600
}

/// Local epoch of `hour:00` on day `d` (DST-safe: a skipped hour moves to the next valid one).
fn local_epoch(d: NaiveDate, hour: u32) -> Option<i64> {
    let ndt = d.and_hms_opt(hour.min(23), 0, 0)?;
    Local
        .from_local_datetime(&ndt)
        .earliest()
        .or_else(|| {
            Local
                .from_local_datetime(&(ndt + chrono::Duration::hours(1)))
                .earliest()
        })
        .map(|t| t.timestamp())
}

/// When a backup dir becomes due again under `plan`.
/// `None` = never by itself (manual), `Some(i64::MIN)` = due right now.
///
/// A sync belongs to the "slot" (planned day at `hour`) at or before it, so a run that started on
/// its planned day and finished after midnight does not cause the next planned day to be skipped.
pub fn plan_due_at(plan: Plan, hour: u32, last_commit: Option<i64>) -> Option<i64> {
    let lc = match plan {
        Plan::Manual => return None,
        Plan::Continuous => return Some(i64::MIN),
        _ => match last_commit {
            None => return Some(i64::MIN), // never copied: get a first copy now
            Some(t) => t,
        },
    };
    let slot_day = Local.timestamp_opt(lc, 0).single()?.naive_local()
        - chrono::Duration::hours(hour.min(23) as i64);
    let last_day = slot_day.date();
    let day = match plan {
        Plan::EveryDays(n) => last_day + chrono::Duration::days(n.clamp(1, 365) as i64),
        Plan::Weekdays(m) => {
            let m = if m & 0x7f == 0 { 0x7f } else { m };
            let mut d = last_day.succ_opt()?;
            while m & (1 << d.weekday().num_days_from_monday()) == 0 {
                d = d.succ_opt()?;
            }
            d
        }
        Plan::Continuous | Plan::Manual => unreachable!(),
    };
    local_epoch(day, hour)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(s: &str) -> i64 {
        let n = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
        Local
            .from_local_datetime(&n)
            .earliest()
            .unwrap()
            .timestamp()
    }
    #[test]
    fn plans() {
        assert_eq!(
            plan_due_at(Plan::Continuous, 0, Some(at("2026-11-02 10:00"))),
            Some(i64::MIN)
        );
        assert_eq!(
            plan_due_at(Plan::Manual, 0, Some(at("2026-11-02 10:00"))),
            None
        );
        assert_eq!(plan_due_at(Plan::EveryDays(1), 0, None), Some(i64::MIN));
        // daily, synced Mon 10:00 -> due Tue 00:00
        assert_eq!(
            plan_due_at(Plan::EveryDays(1), 0, Some(at("2026-11-02 10:00"))),
            Some(at("2026-11-03 00:00"))
        );
        // every 3 days from 22:00; a run that finished 01:30 belongs to the previous evening's slot
        assert_eq!(
            plan_due_at(Plan::EveryDays(3), 22, Some(at("2026-11-03 01:30"))),
            Some(at("2026-11-05 22:00"))
        );
        assert_eq!(
            plan_due_at(Plan::EveryDays(3), 22, Some(at("2026-11-02 23:00"))),
            Some(at("2026-11-05 22:00"))
        );
        // Mon+Thu (bits 0 and 3); 2026-11-02 is a Monday
        let m = 0b0000_1001;
        assert_eq!(
            plan_due_at(Plan::Weekdays(m), 0, Some(at("2026-11-02 09:00"))),
            Some(at("2026-11-05 00:00"))
        );
        assert_eq!(
            plan_due_at(Plan::Weekdays(m), 6, Some(at("2026-11-05 07:00"))),
            Some(at("2026-11-09 06:00"))
        );
        // empty mask behaves like every day
        assert_eq!(
            plan_due_at(Plan::Weekdays(0), 0, Some(at("2026-11-02 09:00"))),
            Some(at("2026-11-03 00:00"))
        );
    }
    fn f(name: &str, mtime: i64) -> RemoteFile {
        RemoteFile {
            name: name.into(),
            size: 1,
            mtime,
        }
    }
    #[test]
    fn job_estimation() {
        // server mtimes in CET shown as 05:31 for a 06:30 (EET) job start -> tz offset +2h vs UTC
        let ep = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp();
        let files = vec![
            f(
                "101_DATABASE.19D2026-10-28T063009_9051.vbk",
                ep("2026-10-31T05:32:00+01:00"),
            ),
            f(
                "101_DATABASE.19D2026-10-29T063010_983F.vib",
                ep("2026-10-29T05:31:00+01:00"),
            ),
            f(
                "101_DATABASE.19D2026-10-30T063009_C53C.vib",
                ep("2026-10-30T05:31:00+01:00"),
            ),
            f(
                "101_DATABASE.19D2026-10-31T063004_BDE1.vib",
                ep("2026-10-31T05:31:00+01:00"),
            ),
            f("101_DATABASE_74B83.vbm", ep("2026-10-31T05:32:00+01:00")),
        ];
        let j = estimate(&files).unwrap();
        assert_eq!(j.last_start, ep("2026-10-31T06:30:04+02:00"));
        assert!(j.max_dur >= 60 && j.max_dur < 300, "{}", j.max_dur);
        let start = j.last_start + DAY;
        assert!(j.blocked_until(start - 60, 1800, 1200).is_some());
        assert!(j.blocked_until(start - 4 * 3600, 1800, 1200).is_none());
    }
}
