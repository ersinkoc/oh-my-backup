//! Offline UI review fixtures. No SSH connection, sampler, config writes or transfers.
use crate::i18n;
use crate::{
    config::Plan,
    remote::RemoteFile,
    sync::{Activity, Control, LocalFile, VmView},
};

pub fn populate(ctl: &Control) {
    let now = chrono::Utc::now().timestamp();
    let mut s = ctl.shared.lock().unwrap();
    s.conn = i18n::text("Çevrimdışı önizleme · örnek veriler").into();
    s.waiting = i18n::text("Önizleme: sunucuya bağlanılmaz ve dosya aktarılmaz").into();
    s.last_scan = Some(now);
    s.local_total = 4 << 40;
    s.local_free = 1800 << 30;
    s.remote_disk = Some((8 << 40, 3 << 40));
    for (i, name) in [
        "01_DATABASE",
        "02_FILESERVER",
        "03_APPLICATION",
        "04_ARCHIVE",
        "05_MONITORING",
        "06_MAIL",
    ]
    .iter()
    .enumerate()
    {
        let size = (i as u64 + 1) * (80 << 30);
        let synced = i == 0 || i == 3;
        s.vms.push(VmView {
            name: name.to_string(),
            remote: vec![RemoteFile {
                name: format!("{name}.vbk"),
                size,
                mtime: now - 3600,
            }],
            remote_size: size,
            busy: i == 4,
            job: None,
            local: if synced {
                vec![LocalFile {
                    name: format!("{name}.vbk"),
                    size,
                }]
            } else {
                vec![]
            },
            local_size: if synced { size } else { 0 },
            local_exists: synced,
            committed_at: synced.then_some(now - 1800),
            committed: vec![],
            in_sync: synced,
            incomplete: false,
            excluded: i == 5,
            legacy: vec![],
            staging_files: vec![],
            status: if synced {
                i18n::text("Güncel ✓")
            } else if i == 4 {
                i18n::text("Sunucuda job çalışıyor")
            } else if i == 5 {
                i18n::text("Hariç tutuldu")
            } else {
                i18n::text("İndirilecek")
            }
            .into(),
            last_error: (i == 2)
                .then(|| i18n::text("Örnek: bağlantı kesildi; yeniden denenecek").into()),
            est_bytes: if synced { 0 } else { size / 4 },
            plan: Plan::Continuous,
            plan_custom: false,
            due_at: Some(i64::MIN),
            retry_at: (i == 2).then_some(now + 300),
            snoozed: false,
        });
    }
    s.activity = Some(Activity {
        vm: "02_FILESERVER".into(),
        phase: i18n::text("İndiriliyor").into(),
        file: "02_FILESERVER.vbk".into(),
        done: 12 << 30,
        total: 40 << 30,
        speed: 28e6,
        started: now - 480,
        net_bytes: 12 << 30,
        bg: String::new(),
    });
    s.speed_hist = (0..600)
        .map(|i| 24e6 + (i as f32 / 15.0).sin() * 6e6)
        .collect();
    s.session_bytes = 32 << 30;
    s.session_start = now - 1800;
    s.session_active_s = 1200;
    s.stats = Default::default();
    s.stats.total_bytes = 1250 << 30;
    s.stats.active_secs = 86400;
    s.log
        .push_back(i18n::text("12:00:00 ✔ 01_DATABASE: tamamlandı (örnek)").into());
    s.log
        .push_back(i18n::text("12:01:00 ▶ 02_FILESERVER: senkron başlıyor (örnek)").into());
    s.log
        .push_back(i18n::text("12:02:00 ✖ 03_APPLICATION: bağlantı hatası (örnek)").into());
}
