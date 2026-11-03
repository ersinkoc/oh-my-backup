use crate::config::{Config, Plan, UpdateMode, WEEKDAYS};
use crate::i18n;
use crate::rules::ProfileKind;
use crate::schedule::{self, fmt_time};
use crate::sync::{Activity, Control, VmView, dur_h, human};
use chrono::{Local, TimeZone, Timelike};
use eframe::egui::{self, Color32, RichText};
use egui::{Rect, Sense, Shape, Stroke, pos2, vec2};
use egui_extras::{Column, TableBuilder};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;

const GREEN: Color32 = Color32::from_rgb(72, 223, 192);
const RED: Color32 = Color32::from_rgb(255, 126, 138);
const AMBER: Color32 = Color32::from_rgb(225, 170, 60);
const BLUE: Color32 = Color32::from_rgb(111, 187, 255);

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct App {
    ctl: Arc<Control>,
    hub: crate::profiles::Hub,
    primary: Arc<Control>,
    profiles_open: bool,
    profile_name: String,
    new_kind: crate::rules::ProfileKind,
    rename_profile: String,
    pending_archive: Option<String>,
    rules_draft: Option<RulesDraft>,
    selected: Option<String>,
    settings: Option<Draft>,
    confirm_approve_all: bool,
    sort: (SortCol, bool),
    chart_window: usize,
    show_dash: bool,
    search: String,
    filter: Filter,
    log_filter: LogFilter,
    last_title: String,
    preview: bool,
    logo: Option<egui::TextureHandle>,
    notice: Option<String>,
    confirm_exit: bool,
    closing: bool,
    updater: crate::update::Updater,
    update_open: bool,
    update_token: String,
    update_pending: Option<crate::update::Prepared>,
}

struct RulesDraft {
    rules: crate::rules::FileRules,
    includes: String,
    excludes: String,
    delete_confirmed: bool,
    error: Option<String>,
    preview: Vec<crate::generic::Item>,
}

/// Settings being edited (applied only on "Kaydet").
struct Draft {
    cfg: Config,
    pw: String,
    autostart: bool,
    autostart_orig: bool,
    err: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortCol {
    Name,
    Remote,
    Last,
    Job,
    Local,
    Progress,
    Synced,
    Next,
    Legacy,
    Status,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Pending,
    Errors,
    Synced,
    Excluded,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogFilter {
    All,
    Important,
    Errors,
}

pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Ok(b) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts
            .font_data
            .insert("segoe".into(), Arc::new(egui::FontData::from_owned(b)));
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "segoe".into());
    }
    if let Ok(b) = std::fs::read(r"C:\Windows\Fonts\seguisym.ttf") {
        fonts
            .font_data
            .insert("sym".into(), Arc::new(egui::FontData::from_owned(b)));
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("sym".into());
    }
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.global_style()).clone();
    style.visuals = egui::Visuals::dark();
    let v = &mut style.visuals;
    v.panel_fill = Color32::from_rgb(15, 23, 34);
    v.window_fill = Color32::from_rgb(20, 32, 46);
    v.extreme_bg_color = Color32::from_rgb(10, 17, 27);
    v.faint_bg_color = Color32::from_rgb(23, 37, 52);
    v.override_text_color = Some(Color32::from_rgb(224, 235, 246));
    v.selection.bg_fill = Color32::from_rgb(28, 79, 87);
    v.selection.stroke = Stroke::new(1.0, GREEN);
    v.widgets.inactive.bg_fill = Color32::from_rgb(28, 43, 59);
    v.widgets.hovered.bg_fill = Color32::from_rgb(39, 62, 80);
    v.widgets.active.bg_fill = Color32::from_rgb(29, 89, 91);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(44, 62, 80));
    style.spacing.item_spacing = vec2(10.0, 8.0);
    style.spacing.button_padding = vec2(12.0, 7.0);
    style.spacing.interact_size.y = 30.0;
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
    ctx.set_global_style(style);
}

impl App {
    pub fn new(ctl: Arc<Control>, preview: bool) -> App {
        App {
            primary: ctl.clone(),
            hub: crate::profiles::Hub::single(ctl.clone()),
            profiles_open: false,
            profile_name: String::new(),
            new_kind: crate::rules::ProfileKind::Files,
            rename_profile: String::new(),
            pending_archive: None,
            rules_draft: None,
            ctl,
            selected: None,
            settings: None,
            confirm_approve_all: false,
            sort: (SortCol::Name, true),
            chart_window: 600,
            show_dash: true,
            search: String::new(),
            filter: Filter::All,
            log_filter: LogFilter::All,
            last_title: String::new(),
            preview,
            logo: None,
            notice: None,
            confirm_exit: false,
            closing: false,
            updater: crate::update::Updater::default(),
            update_open: false,
            update_token: String::new(),
            update_pending: None,
        }
    }
    pub fn with_profiles(hub: crate::profiles::Hub, preview: bool, notice: Option<String>) -> App {
        let mut app = Self::new(hub.profiles[hub.selected].ctl.clone(), preview);
        app.primary = hub.profiles[0].ctl.clone();
        app.hub = hub;
        app.notice = notice;
        app
    }
    fn select_profile(&mut self, index: usize) {
        if let Err(error) = self.hub.select(index) {
            self.notice = Some(error.to_string());
            return;
        }
        self.ctl = self.hub.profiles[index].ctl.clone();
        self.selected = None;
        self.search.clear();
        self.filter = Filter::All;
        self.last_title.clear();
    }
    fn save_language(&mut self, language: crate::i18n::Language) {
        let mut config = self.primary.config.lock().unwrap();
        let mut next = config.clone();
        next.language = language;
        if !self.preview
            && let Err(error) = next.save()
        {
            self.notice = Some(error.to_string());
            return;
        }
        *config = next;
    }

    fn log(&self, message: impl AsRef<str>) {
        if self.preview {
            self.ctl
                .shared
                .lock()
                .unwrap()
                .log
                .push_back(message.as_ref().into());
        } else {
            self.ctl.log(message);
        }
    }

    fn save_cfg(&mut self, f: impl FnOnce(&mut Config)) {
        let mut c = self.ctl.config.lock().unwrap();
        let mut next = c.clone();
        f(&mut next);
        if !self.preview
            && let Err(e) = next.save()
        {
            self.notice = Some(i18n::choose(
                format!("Ayar kaydedilemedi: {e:#}"),
                format!("Could not save setting: {e:#}"),
            ));
            return;
        }
        *c = next;
        drop(c);
        self.ctl.scan_now.store(true, Ordering::SeqCst);
        self.ctl.wake();
    }
}

fn pct(done: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0) as f32
    }
}

fn is_error_line(l: &str) -> bool {
    let lower = l.to_lowercase();
    l.contains('✖')
        || [
            "hata",
            "error",
            "failed",
            "yetersiz",
            "insufficient",
            "silinemedi",
            "could not",
        ]
        .iter()
        .any(|word| lower.contains(word))
}

fn is_important_line(l: &str) -> bool {
    let lower = l.to_lowercase();
    is_error_line(l)
        || ['▶', '✔', '⏸', '🗑'].iter().any(|c| l.contains(*c))
        || [
            "bağlan",
            "connect",
            "duraklat",
            "paused",
            "başlatıldı",
            "started",
        ]
        .iter()
        .any(|word| lower.contains(word))
}

fn matches_filter(v: &VmView, f: Filter) -> bool {
    match f {
        Filter::All => true,
        Filter::Pending => !v.in_sync && !v.excluded,
        Filter::Errors => v.last_error.is_some() && !v.in_sync,
        Filter::Synced => v.in_sync,
        Filter::Excluded => v.excluded,
    }
}

/// Next planned sync: (sort key, text, color, tooltip)
fn next_info(v: &VmView, now: i64, active: bool, weak: Color32) -> (i64, String, Color32, String) {
    let plan = format!(
        "Plan: {}{}",
        v.plan.label(),
        if v.plan_custom {
            i18n::text(" (bu yedeğe özel)")
        } else {
            ""
        }
    );
    if v.excluded {
        return (
            i64::MAX,
            "—".into(),
            weak,
            i18n::text("Hariç tutuldu").into(),
        );
    }
    if active {
        return (i64::MIN, i18n::text("şimdi").into(), BLUE, plan);
    }
    if v.snoozed {
        return (
            i64::MAX - 2,
            i18n::text("durduruldu").into(),
            AMBER,
            i18n::text("Kullanıcı durdurdu. 'Şimdi senkronla' ya da 'Başlat' ile devam eder.")
                .into(),
        );
    }
    if let Some(u) = v.retry_at {
        return (
            u,
            schedule::hm(u),
            AMBER,
            i18n::choose(
                format!("Hata sonrası tekrar deneme: {}\n{plan}", fmt_time(u)),
                format!("Retry after error: {}\n{plan}", fmt_time(u)),
            ),
        );
    }
    if let Some(action) = v.file_action {
        let reason = i18n::display(v.file_reason.as_deref().unwrap_or(""));
        return (
            if action == crate::generic::Action::Copy {
                now
            } else {
                i64::MAX
            },
            action.label().into(),
            if action == crate::generic::Action::Current {
                GREEN
            } else {
                AMBER
            },
            reason,
        );
    }
    match v.due_at {
        None if !v.incomplete => (i64::MAX - 1, i18n::text("elle").into(), weak, plan),
        Some(u) if u > now && !v.incomplete => {
            (u, fmt_time(u), if v.in_sync { weak } else { AMBER }, plan)
        }
        _ if v.in_sync => (
            i64::MAX - 3,
            i18n::text("yeni yedekte").into(),
            weak,
            i18n::choose(
                format!("{plan}\nSunucuda yeni yedek çıkınca indirilir"),
                format!("{plan}\nDownloads when a new backup appears on the server"),
            ),
        ),
        _ => (
            now,
            i18n::text("sırada").into(),
            GREEN,
            i18n::choose(
                format!("{plan}\nİndirilmeye hazır (job penceresi / saat sınırı dışında başlar)"),
                format!("{plan}\nReady to download (subject to job windows and allowed hours)"),
            ),
        ),
    }
}

/// Plan editor; returns true when the plan was changed.
fn plan_editor(ui: &mut egui::Ui, id: &str, plan: &mut Plan) -> bool {
    let kinds: [&str; 4] = [
        i18n::text("Sürekli (yeni yedek çıktıkça)"),
        i18n::text("Her N günde bir"),
        i18n::text("Haftanın belirli günleri"),
        i18n::text("Sadece elle"),
    ];
    let mut kind = match plan {
        Plan::Continuous => 0,
        Plan::EveryDays(_) => 1,
        Plan::Weekdays(_) => 2,
        Plan::Manual => 3,
    };
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        let mut kind_changed = false;
        egui::ComboBox::from_id_salt(id)
            .width(220.0)
            .selected_text(i18n::text(kinds[kind]))
            .show_ui(ui, |ui| {
                for (k, label) in kinds.iter().enumerate() {
                    kind_changed |= ui
                        .selectable_value(&mut kind, k, i18n::text(label))
                        .changed();
                }
            });
        if kind_changed {
            *plan = match kind {
                0 => Plan::Continuous,
                1 => Plan::EveryDays(1),
                2 => Plan::Weekdays(0b001_1111),
                _ => Plan::Manual,
            };
            changed = true;
        }
        match plan {
            Plan::EveryDays(n) => {
                changed |= ui
                    .add(
                        egui::DragValue::new(n)
                            .range(1..=60)
                            .suffix(i18n::text(" gün")),
                    )
                    .on_hover_text(i18n::text("1 = her gün, 2 = gün aşırı, 7 = haftada bir …"))
                    .changed();
                ui.label(
                    RichText::new(if *n == 1 {
                        i18n::text("her gün")
                    } else {
                        i18n::text("arayla")
                    })
                    .weak(),
                );
            }
            Plan::Weekdays(m) => {
                for (i, d) in WEEKDAYS.iter().enumerate() {
                    let mut on = *m & (1 << i) != 0;
                    if ui.toggle_value(&mut on, i18n::text(d)).changed() {
                        if on {
                            *m |= 1 << i;
                        } else if *m & !(1 << i) != 0 {
                            *m &= !(1 << i); // at least one day stays selected
                        }
                        changed = true;
                    }
                }
            }
            _ => {}
        }
    });
    changed
}

fn validate(c: &Config) -> Result<(), String> {
    c.validate()
}

impl eframe::App for App {
    fn on_exit(&mut self) {
        if !self.preview {
            for profile in &self.hub.profiles {
                profile.ctl.shared.lock().unwrap().stats.save();
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

impl App {
    fn render(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        for profile in &self.hub.profiles {
            let mut current = profile.ctl.ctx.lock().unwrap();
            if current.is_none() {
                *current = Some(ctx.clone());
            }
        }
        if let Some(id) = self.pending_archive.clone()
            && self
                .hub
                .profiles
                .iter()
                .find(|p| p.entry.id == id)
                .is_some_and(|p| {
                    !p.started
                        || (p.ctl.engine_stopped.load(Ordering::SeqCst)
                            && p.ctl.sampler_stopped.load(Ordering::SeqCst))
                })
        {
            if let Err(error) = self.hub.archive(&id) {
                self.notice = Some(error.to_string());
            }
            self.ctl = self.hub.profiles[self.hub.selected].ctl.clone();
            self.selected = None;
            self.pending_archive = None;
        }
        self.updater.poll();
        if let Some(package) = self.update_pending.clone() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.hub.stopped() {
                match self.updater.launch_apply(&package) {
                    Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(error) => {
                        self.updater.state = crate::update::State::Error(error.to_string())
                    }
                }
                self.update_pending = None;
            }
        }
        if self.logo.is_none() {
            self.logo = Some(ctx.load_texture(
                "hbsync-logo",
                egui::ColorImage::from_rgba_unmultiplied(
                    [256, 256],
                    include_bytes!("../assets/icon.rgba"),
                ),
                egui::TextureOptions::LINEAR,
            ));
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
        let ctl = self.ctl.clone();
        let (conn, connected, rdisk, last_scan, vms, others, act, waiting, log, lfree, ltotal, net) = {
            let s = ctl.shared.lock().unwrap();
            let net = NetView {
                hist: s.speed_hist.clone(),
                session_start: s.session_start,
                session_bytes: s.session_bytes,
                session_active_s: s.session_active_s,
                stats: s.stats.clone(),
            };
            (
                s.conn.clone(),
                s.connected,
                s.remote_disk,
                s.last_scan,
                s.vms.clone(),
                s.others.clone(),
                s.activity.clone(),
                s.waiting.clone(),
                s.log.iter().rev().take(600).cloned().collect::<Vec<_>>(),
                s.local_free,
                s.local_total,
                net,
            )
        };
        let mut cfg = ctl.config.lock().unwrap().clone();
        cfg.language = self.primary.config.lock().unwrap().language;
        i18n::set_language(cfg.language);
        let running = ctl.running.load(Ordering::SeqCst);
        if ctx.input(|i| i.viewport().close_requested()) && !self.hub.stopped() && !self.preview {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.closing {
                if self.hub.busy() {
                    self.confirm_exit = true;
                } else {
                    self.hub.request_shutdown();
                    self.closing = true;
                }
            }
        }
        if self.closing && self.hub.stopped() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let now = chrono::Utc::now().timestamp();
        let n_sync = vms.iter().filter(|v| v.in_sync).count();

        // window title shows progress, also visible in the taskbar
        let title = match &act {
            Some(a) => format!(
                "Oh My Backup — ⇣ {} {:.0}%",
                a.vm,
                pct(a.done, a.total) * 100.0
            ),
            None => i18n::choose(
                format!("Oh My Backup — {n_sync}/{} güncel", vms.len()),
                format!("Oh My Backup — {n_sync}/{} up to date", vms.len()),
            ),
        };
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }

        // ------------------------------------------------ top
        egui::Panel::top("top").show_inside(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let can_switch = self.settings.is_none() && self.rules_draft.is_none() && self.update_pending.is_none() && self.pending_archive.is_none();
                let mut selected = self.hub.selected;
                ui.add_enabled_ui(can_switch, |ui| {
                    egui::ComboBox::from_id_salt("profile-selector").width(200.0).selected_text(&self.hub.profiles[selected].entry.name).show_ui(ui, |ui| {
                        for (index, profile) in self.hub.profiles.iter().enumerate() { ui.selectable_value(&mut selected, index, &profile.entry.name); }
                    });
                    if ui.button(i18n::choose("Profiller", "Profiles")).clicked() { self.rename_profile = self.hub.profiles[self.hub.selected].entry.name.clone(); self.profiles_open = true; }
                });
                if selected != self.hub.selected { self.select_profile(selected); ctx.request_repaint(); }
                ui.label(match cfg.kind { crate::rules::ProfileKind::Veeam => "Veeam", crate::rules::ProfileKind::Files => i18n::choose("Dosya otomasyonu", "File automation") });
                if cfg.kind == crate::rules::ProfileKind::Files && ui.add_enabled(self.settings.is_none() && self.update_pending.is_none(), egui::Button::new(i18n::choose("Kurallar ve önizleme", "Rules and preview"))).clicked() {
                    self.rules_draft = Some(RulesDraft { rules: cfg.rules.clone(), includes: cfg.rules.include.join("\n"), excludes: cfg.rules.exclude.join("\n"), delete_confirmed: cfg.rules.deletion == crate::rules::Deletion::Mirror, error: None, preview: Vec::new() });
                }
                ui.label(format!("{} {}", self.hub.profiles.len(), i18n::choose("profil", "profiles")));
            });
            ui.horizontal_wrapped(|ui| {
                if let Some(logo) = &self.logo { ui.image((logo.id(), vec2(44.0, 44.0))); }
                ui.vertical(|ui| {
                    ui.label(RichText::new("Oh My Backup").size(24.0).strong());
                    ui.label(RichText::new("BACKUP & SYNC  /  HBSYNC").size(10.0).color(GREEN));
                });
                ui.separator();
                ui.label(RichText::new("●").color(if connected { GREEN } else { RED }));
                ui.label(i18n::display(&conn));
                if let Some((t, f)) = rdisk {
                    ui.separator();
                    ui.label(i18n::choose(format!("Sunucu: {} dolu / {}", human(t.saturating_sub(f)), human(t)), format!("Server: {} used / {}", human(t.saturating_sub(f)), human(t))));
                }
                ui.separator();
                let low = ltotal > 0 && lfree < (cfg.min_free_gb << 30) + (100 << 30);
                ui.label(RichText::new(if ltotal == 0 { i18n::choose(format!("{} · disk bilgisi bekleniyor", cfg.local_root), format!("{} · waiting for disk information", cfg.local_root)) } else { i18n::choose(format!("{}: {} boş / {}", cfg.local_root, human(lfree), human(ltotal)), format!("{}: {} free / {}", cfg.local_root, human(lfree), human(ltotal))) }).color(if low { AMBER } else { ui.visuals().text_color() }));
                if let Some(t) = last_scan {
                    ui.separator();
                    ui.label(i18n::choose(format!("Son tarama {}", schedule::hm(t)), format!("Last scan {}", schedule::hm(t))));
                }
            });
            ui.horizontal_wrapped(|ui| {
                if running {
                    if ui.button(i18n::text("⏸ Duraklat")).on_hover_text(i18n::text("Otomatik senkronu durdurur; yarım işler korunur")).clicked() {
                        ctl.running.store(false, Ordering::SeqCst);
                        self.log(i18n::text("Duraklatıldı (kullanıcı)"));
                        ctl.wake();
                    }
                } else if ui.button(i18n::text("▶ Başlat")).on_hover_text(i18n::text("Otomatik senkronu başlatır (durdurulan yedekler de devam eder)")).clicked() {
                    ctl.snoozed.lock().unwrap().clear();
                    ctl.running.store(true, Ordering::SeqCst);
                    self.log(i18n::text("Otomatik senkron başlatıldı"));
                    ctl.scan_now.store(true, Ordering::SeqCst);
                    ctl.wake();
                }
                if ui.add_enabled(!self.preview, egui::Button::new(i18n::text("⟳ Şimdi tara"))).clicked() {
                    ctl.scan_now.store(true, Ordering::SeqCst);
                    ctl.wake();
                }
                if act.is_some()
                    && ui
                        .button(i18n::text("⏹ Bu işi durdur"))
                        .on_hover_text(i18n::text("Mevcut yedeğin senkronunu durdurur (ilerleme korunur). Bu yedek, 'Şimdi senkronla' ya da 'Başlat' denene kadar otomatik başlamaz; diğerleri devam eder."))
                        .clicked()
                {
                    ctl.stop_current();
                    self.log(i18n::text("Mevcut iş durduruldu (kullanıcı)"));
                }
                if ui.selectable_label(self.show_dash, i18n::text("📊 Panel")).clicked() {
                    self.show_dash = !self.show_dash;
                }
                let mut language = cfg.language;
                egui::ComboBox::from_id_salt("display-language").width(92.0).selected_text(language.label()).show_ui(ui, |ui| {
                    ui.selectable_value(&mut language, crate::i18n::Language::Turkish, "Türkçe");
                    ui.selectable_value(&mut language, crate::i18n::Language::English, "English");
                });
                if language != cfg.language {
                    self.save_language(language);
                    ctx.request_repaint();
                }
                if ui.add_enabled(self.rules_draft.is_none() && self.update_pending.is_none(), egui::Button::new(i18n::text("⚙ Ayarlar"))).clicked() {
                    let a = !self.preview && autostart_enabled();
                    self.settings = Some(Draft { cfg: cfg.clone(), pw: String::new(), autostart: a, autostart_orig: a, err: None });
                }
                if ui.button(i18n::choose("↻ Güncellemeler", "↻ Updates")).clicked() { self.update_open = true; }
                ui.separator();
                let mode = match cfg.mode {
                    UpdateMode::InPlace => i18n::text("Mod: yerinde"),
                    UpdateMode::Safe => i18n::text("Mod: güvenli"),
                };
                ui.label(RichText::new(mode).weak());
                let plan_txt = match cfg.plan {
                    Plan::Continuous | Plan::Manual => format!("Plan: {}", cfg.plan.label()),
                    p => i18n::choose(format!("Plan: {} · {:02}:00'dan sonra", p.label(), cfg.plan_hour), format!("Plan: {} · after {:02}:00", p.label(), cfg.plan_hour)),
                };
                ui.label(RichText::new(plan_txt).weak());
                if let Some(u) = vms.iter().filter(|v| !v.excluded).filter_map(|v| v.due_at).filter(|u| *u > now).min()
                    && cfg.plan != Plan::Continuous
                {
                    ui.label(RichText::new(i18n::choose(format!("sonraki planlı: {}", fmt_time(u)), format!("next scheduled: {}", fmt_time(u)))).weak());
                }
                ui.label(RichText::new(if running { i18n::text("Otomatik: AÇIK") } else { i18n::text("Otomatik: KAPALI") }).color(if running { GREEN } else { AMBER }));
            });
            ui.add_space(4.0);
            if self.preview { ui.colored_label(AMBER, i18n::text("ÖNİZLEME · Örnek veriler. Gerçek bağlantı ve dosya aktarımı kapalı.")); }
            if let Some(message) = self.notice.clone() {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(RED, message);
                    if ui.small_button(i18n::text("Kapat")).clicked() { self.notice = None; }
                });
            }
            if self.show_dash {
                let speed_est = *ctl.speed_est.lock().unwrap();
                self.dashboard(ui, &net, &vms, act.as_ref(), &cfg, speed_est);
            }
            ui.add_space(2.0);
            // activity
            match &act {
                Some(a) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("⇣ {}", a.vm)).strong().color(BLUE));
                        ui.label(if a.file.is_empty() { i18n::display(&a.phase) } else { format!("{} — {}", i18n::display(&a.phase), a.file) });
                        ui.label(RichText::new(i18n::choose(format!("· {} sürüyor · bu işte {} indirildi", dur_h(now - a.started), human(a.net_bytes)), format!("· elapsed {} · downloaded {} in this job", dur_h(now - a.started), human(a.net_bytes)))).weak());
                    });
                    let mut text = if a.total > 0 { format!("{} / {}", human(a.done), human(a.total)) } else { "…".to_string() };
                    if (a.phase == "İndiriliyor" || a.phase == "Downloading") && a.speed > 0.0 {
                        let eta = (a.total.saturating_sub(a.done)) as f64 / a.speed;
                        text += &format!("   {:.1} MB/s   kalan ~{}", a.speed / 1e6, dur_h(eta as i64));
                    }
                    let bar = egui::ProgressBar::new(pct(a.done, a.total)).text(text);
                    ui.add(if a.total == 0 { bar.animate(true) } else { bar });
                    if !a.bg.is_empty() {
                        ui.label(RichText::new(format!("⟲ {}", i18n::display(&a.bg))).size(11.5).weak())
                            .on_hover_text(i18n::text("Değişen blokları bulmak için sunucu .vbk'yi kendi diskinde okuyor; bu sırada diğer dosyalar indiriliyor. Yarıda kalırsa kaldığı yerden devam eder."));
                    }
                }
                None => {
                    ui.label(RichText::new(if waiting.is_empty() { i18n::text("Bağlantı ve yedek bilgileri hazırlanıyor…").into() } else { format!("⏳ {}", i18n::display(&waiting)) }).weak());
                }
            }
            ui.add_space(4.0);
        });

        // ------------------------------------------------ log
        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(110.0)
            .min_size(60.0)
            .max_size(ctx.content_rect().height() * 0.4)
            .show_inside(ui, |ui| {
                let shown: Vec<&String> = log
                    .iter()
                    .filter(|l| match self.log_filter {
                        LogFilter::All => true,
                        LogFilter::Important => is_important_line(l),
                        LogFilter::Errors => is_error_line(l),
                    })
                    .collect();
                ui.horizontal(|ui| {
                    ui.strong(i18n::text("Günlük"));
                    ui.selectable_value(&mut self.log_filter, LogFilter::All, i18n::text("Tümü"));
                    ui.selectable_value(
                        &mut self.log_filter,
                        LogFilter::Important,
                        i18n::text("Önemli"),
                    );
                    ui.selectable_value(
                        &mut self.log_filter,
                        LogFilter::Errors,
                        i18n::text("Hatalar"),
                    );
                    ui.separator();
                    if ui
                        .small_button(i18n::text("Kopyala"))
                        .on_hover_text(i18n::text("Gösterilen satırları panoya kopyalar"))
                        .clicked()
                    {
                        let txt: Vec<&str> = shown.iter().rev().map(|s| s.as_str()).collect();
                        ui.ctx().copy_text(txt.join("\n"));
                    }
                    if ui.small_button(i18n::text("Log klasörü")).clicked() {
                        let _ = std::process::Command::new("explorer")
                            .arg(self.ctl.logs_dir())
                            .spawn();
                    }
                });
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        if shown.is_empty() {
                            ui.label(RichText::new(i18n::text("(kayıt yok)")).weak());
                        }
                        for l in &shown {
                            let c = if is_error_line(l) {
                                RED
                            } else if l.contains('✔') || l.contains("🗑") {
                                GREEN
                            } else if l.contains('⏸') {
                                AMBER
                            } else {
                                ui.visuals().text_color()
                            };
                            ui.label(RichText::new(l.as_str()).monospace().size(12.0).color(c));
                        }
                    });
            });

        // ------------------------------------------------ details
        let sel = self
            .selected
            .clone()
            .and_then(|s| vms.iter().find(|v| v.name == s).cloned());
        if ctx.content_rect().width() >= 1250.0 && sel.is_some() {
            egui::Panel::right("detail")
                .resizable(true)
                .default_size(380.0)
                .min_size(280.0)
                .max_size(520.0)
                .show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong(i18n::text("Yedek ayrıntıları"));
                        if ui.button(i18n::text("Kapat ×")).clicked() {
                            self.selected = None;
                        }
                    });
                    egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                        if let Some(v) = &sel {
                            self.detail(
                                ui,
                                v,
                                &cfg,
                                act.as_ref().is_some_and(|a| a.vm == v.name),
                                now,
                            );
                        }
                    });
                });
        } else if let Some(v) = &sel {
            let mut open = true;
            egui::Window::new(i18n::text("Yedek ayrıntıları"))
                .open(&mut open)
                .default_width(580.0)
                .show(&ctx, |ui| {
                    egui::ScrollArea::both()
                        .max_height(ctx.content_rect().height() - 140.0)
                        .show(ui, |ui| {
                            self.detail(
                                ui,
                                v,
                                &cfg,
                                act.as_ref().is_some_and(|a| a.vm == v.name),
                                now,
                            );
                        });
                });
            if !open {
                self.selected = None;
            }
        }

        // ------------------------------------------------ table
        egui::CentralPanel::default_margins().show_inside(ui, |ui| {
            let total_remote: u64 = vms.iter().map(|v| v.remote_size).sum();
            ui.horizontal_wrapped(|ui| {
                ui.strong(i18n::choose(format!("{}: {}  ({} güncel / {})", if cfg.kind == ProfileKind::Files { "Kaynak dosyaları" } else { "Sunucudaki yedekler" }, human(total_remote), n_sync, vms.len()), format!("{}: {}  ({} up to date / {})", if cfg.kind == ProfileKind::Files { "Source files" } else { "Server backups" }, human(total_remote), n_sync, vms.len())));
                let unapproved = vms.iter().filter(|v| !v.legacy.is_empty()).any(|v| {
                    let ap = cfg.legacy_approved.get(&v.name).cloned().unwrap_or_default();
                    v.legacy.iter().any(|(n, _)| !ap.contains(n))
                });
                if unapproved && ui.button(i18n::text("Eski dizin eşleşmelerinin tümünü onayla…")).clicked() {
                    self.confirm_approve_all = true;
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(i18n::text("🔍 Ara…")).desired_width(180.0));
                if !self.search.is_empty() && ui.small_button("✖").clicked() {
                    self.search.clear();
                }
                ui.separator();
                let count = |f: Filter| vms.iter().filter(|v| matches_filter(v, f)).count();
                for (f, label) in [(Filter::All, i18n::text("Tümü")), (Filter::Pending, i18n::text("Bekleyen")), (Filter::Errors, i18n::text("Hatalı")), (Filter::Synced, i18n::text("Güncel")), (Filter::Excluded, i18n::text("Hariç"))] {
                    let n = count(f);
                    if f != Filter::All && n == 0 && self.filter != f {
                        continue;
                    }
                    ui.selectable_value(&mut self.filter, f, format!("{label} ({n})"));
                }
            });
            ui.add_space(4.0);
            let weak = ui.visuals().weak_text_color();
            let needle = self.search.trim().to_uppercase();
            let (col, asc) = self.sort;
            let mut order: Vec<usize> = (0..vms.len())
                .filter(|&i| matches_filter(&vms[i], self.filter) && (needle.is_empty() || vms[i].name.to_uppercase().contains(&needle)))
                .collect();
            order.sort_by(|&a, &b| {
                let (x, y) = (&vms[a], &vms[b]);
                let ax = act.as_ref();
                let o = match col {
                    SortCol::Name => x.name.cmp(&y.name),
                    SortCol::Remote => x.remote_size.cmp(&y.remote_size),
                    SortCol::Last => last_backup(x).cmp(&last_backup(y)),
                    SortCol::Job => next_job(x, now).cmp(&next_job(y, now)),
                    SortCol::Local => x.local_size.cmp(&y.local_size),
                    SortCol::Progress => vm_progress(x, ax).total_cmp(&vm_progress(y, ax)),
                    SortCol::Synced => x.committed_at.cmp(&y.committed_at),
                    SortCol::Next => next_info(x, now, ax.is_some_and(|a| a.vm == x.name), weak).0.cmp(&next_info(y, now, ax.is_some_and(|a| a.vm == y.name), weak).0),
                    SortCol::Legacy => legacy_size(x).cmp(&legacy_size(y)),
                    SortCol::Status => status_rank(x, ax).cmp(&status_rank(y, ax)),
                };
                o.then_with(|| x.name.cmp(&y.name))
            });
            if !asc {
                order.reverse();
            }
            if order.is_empty() {
                ui.add_space(20.0);
                ui.heading(if vms.is_empty() { i18n::text("Yedekleriniz burada görünecek") } else { i18n::text("Eşleşen yedek bulunamadı") });
                ui.label(if vms.is_empty() {
                    if !connected { i18n::text("Bağlantı bilgilerini Ayarlar'dan kontrol edin. Bağlantı kurulduğunda yedekler otomatik listelenir.") } else { i18n::text("Sunucu dizininde henüz bir yedek bulunamadı. Dizin yolunu kontrol edip yeniden tarayın.") }
                } else { i18n::text("Aramayı temizleyin veya başka bir durum filtresi seçin.") });
                if !others.is_empty() { ui.label(i18n::choose(format!("{} yerel dizinin sunucuda karşılığı yok; bu dizinlere dokunulmaz.", others.len()), format!("{} local folders have no server match; these folders are left untouched.", others.len()))); }
                return;
            }
            let mut sort = self.sort;
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            egui::ScrollArea::horizontal().id_salt("backup-table-horizontal").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_min_width(1160.0);
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click())
                .column(Column::initial(200.0).at_least(120.0))
                .column(Column::initial(70.0))
                .column(Column::initial(85.0))
                .column(Column::initial(85.0))
                .column(Column::initial(70.0))
                .column(Column::initial(110.0))
                .column(Column::initial(85.0))
                .column(Column::initial(85.0))
                .column(Column::initial(100.0))
                .column(Column::remainder().at_least(160.0))
                .header(32.0, |mut h| {
                    let cols = [
                        (i18n::text("Sunucu dizini"), SortCol::Name, i18n::text("Ada göre")),
                        (i18n::text("Sunucu"), SortCol::Remote, i18n::text("Sunucudaki toplam boyut")),
                        (if cfg.kind == ProfileKind::Files { i18n::choose("Son değişiklik", "Last modified") } else { i18n::text("Son yedek") }, SortCol::Last, i18n::text("Sunucudaki en yeni dosyanın zamanı")),
                        (i18n::text("Job saati"), SortCol::Job, i18n::text("En yakın job önce")),
                        (i18n::text("Yerel"), SortCol::Local, i18n::text("Yerel diskteki boyut")),
                        (i18n::text("İlerleme"), SortCol::Progress, i18n::text("Güncel olma oranı (indirilmeyenlerde tahmini)")),
                        (i18n::text("Son senkron"), SortCol::Synced, i18n::text("Son başarılı senkron")),
                        (i18n::text("Sonraki"), SortCol::Next, i18n::text("Plana göre bir sonraki senkron")),
                        (i18n::text("Eski yerel"), SortCol::Legacy, "Silinecek eski yerel dizinler"),
                        (i18n::text("Durum"), SortCol::Status, i18n::text("Önce aktif / hatalı / bekleyen")),
                    ];
                    for (t, c, tip) in cols {
                        h.col(|ui| {
                            let arrow = if sort.0 == c { if sort.1 { " ▲" } else { " ▼" } } else { "" };
                            let txt = RichText::new(format!("{t}{arrow}")).strong();
                            let txt = if sort.0 == c { txt.color(BLUE) } else { txt };
                            if ui.add(egui::Button::new(txt).frame(false)).on_hover_text(i18n::choose(format!("{tip} — tıkla: sırala"), format!("{tip} — click to sort"))).clicked() {
                                if sort.0 == c {
                                    sort.1 = !sort.1;
                                } else {
                                    // sizes / dates: biggest-newest first feels natural
                                    sort = (c, matches!(c, SortCol::Name | SortCol::Job | SortCol::Status | SortCol::Progress | SortCol::Next));
                                }
                            }
                        });
                    }
                })
                .body(|body| {
                        body.rows(34.0, order.len(), |mut row| {
                            let i = order[row.index()];
                            let v = &vms[i];
                            let active = act.as_ref().is_some_and(|a| a.vm == v.name);
                            row.set_selected(self.selected.as_deref() == Some(v.name.as_str()));
                            row.col(|ui| {
                                let mut t = RichText::new(&v.name);
                                if active {
                                    t = t.color(BLUE).strong();
                                } else if v.excluded {
                                    t = t.weak();
                                }
                                ui.label(t);
                            });
                            row.col(|ui| {
                                ui.label(human(v.remote_size));
                            });
                            row.col(|ui| {
                                ui.label(fmt_time(last_backup(v)));
                            });
                            row.col(|ui| {
                                let txt = v.job.as_ref().map(|j| j.describe()).unwrap_or_else(|| "?".into());
                                let soon = next_job(v, now) < 2 * 3600;
                                let r = ui.label(if soon { RichText::new(txt).color(AMBER) } else { RichText::new(txt) });
                                if let Some(j) = &v.job {
                                    r.on_hover_text(i18n::choose(format!(
                                        "Sonraki job ~{} sonra, en uzun süre {} ({} restore point'ten tahmin)",
                                        dur_h(next_job(v, now).max(60)),
                                        dur_h(j.max_dur),
                                        j.samples
                                    ), format!(
                                        "Next job in ~{}, longest duration {} (estimated from {} restore points)",
                                        dur_h(next_job(v, now).max(60)),
                                        dur_h(j.max_dur),
                                        j.samples
                                    )));
                                }
                            });
                            row.col(|ui| {
                                ui.label(if v.local_exists { human(v.local_size) } else { "—".into() });
                            });
                            row.col(|ui| {
                                let p = vm_progress(v, act.as_ref());
                                let (c, txt) = if active {
                                    (BLUE, format!("{:.0}%", p * 100.0))
                                } else if v.in_sync {
                                    (GREEN, "✓".to_string())
                                } else if v.committed_at.is_none() && p == 0.0 {
                                    (AMBER, i18n::text("hiç").to_string())
                                } else {
                                    (AMBER, format!("~{:.0}%", p * 100.0))
                                };
                                if p <= 0.0 && !active {
                                    ui.label(RichText::new(txt).color(c));
                                } else {
                                    ui.add(egui::ProgressBar::new(p).fill(c).desired_height(16.0).text(RichText::new(txt).size(11.0)));
                                }
                            });
                            row.col(|ui| {
                                ui.label(v.committed_at.map(fmt_time).unwrap_or_else(|| i18n::text("hiç").into()));
                            });
                            row.col(|ui| {
                                let (_, txt, c, tip) = next_info(v, now, active, weak);
                                ui.label(RichText::new(txt).color(c)).on_hover_text(tip);
                            });
                            row.col(|ui| {
                                if v.legacy.is_empty() {
                                    ui.label("—");
                                } else {
                                    let ap = cfg.legacy_approved.get(&v.name).cloned().unwrap_or_default();
                                    let all = v.legacy.iter().all(|(n, _)| ap.contains(n));
                                    ui.label(RichText::new(i18n::choose(format!("{} dizin, {}", v.legacy.len(), human(legacy_size(v))), format!("{} folders, {}", v.legacy.len(), human(legacy_size(v))))).color(if all { GREEN } else { AMBER }))
                                        .on_hover_text(if all { i18n::text("Senkron bitince silinecek (onaylı)") } else { i18n::text("Onay bekliyor — ayrıntıdan onaylayın") });
                                }
                            });
                            row.col(|ui| {
                                let (txt, c) = if active {
                                    (act.as_ref().map(|a| format!("⇣ {} {:.0}%", i18n::display(&a.phase), pct(a.done, a.total) * 100.0)).unwrap_or_default(), BLUE)
                                } else if v.last_error.is_some() && !v.in_sync {
                                    (format!("✖ {}", i18n::display(v.last_error.as_deref().unwrap_or_default())), RED)
                                } else if let Some(action) = v.file_action {
                                    (format!("{} · {}", action.label(), i18n::display(v.file_reason.as_deref().unwrap_or(""))), if action == crate::generic::Action::Current { GREEN } else { AMBER })
                                } else if v.in_sync {
                                    (i18n::display(&v.status), GREEN)
                                } else if v.excluded || matches!(v.plan, Plan::Manual) || v.due_at.is_some_and(|u| u > now) {
                                    (i18n::display(&v.status), weak)
                                } else {
                                    (i18n::display(&v.status), AMBER)
                                };
                                let mut t = txt;
                                if v.incomplete && !v.in_sync {
                                    t = i18n::choose(format!("⚠ yerel set güncelleniyor · {t}"), format!("⚠ local set updating · {t}"));
                                }
                                ui.label(RichText::new(&t).color(c)).on_hover_text(t);
                            });
                            if row.response().clicked() {
                                self.selected = Some(v.name.clone());
                            }
                        });
                });
            });
            self.sort = sort;
        });

        if self.confirm_approve_all {
            egui::Window::new(i18n::text("Eski dizinleri silme izni")).collapsible(false).resizable(true).default_width(560.0).vscroll(true).show(&ctx, |ui| {
                ui.label(i18n::text("Aşağıdaki yerel dizinlerin silinmesine izin veriyorsunuz. Normalde başarılı senkron sonrası silinir. DİKKAT: Disk yetmezse ve “önce alan aç” ayarı açıksa indirmeden ÖNCE silinebilir:"));
                ui.add_space(4.0);
                for v in vms.iter().filter(|v| !v.legacy.is_empty()) {
                    for (n, s) in &v.legacy {
                        ui.label(format!("  {n} ({})  ←  {}", human(*s), v.name));
                    }
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new(i18n::text("Evet, hepsini onayla")).color(AMBER)).clicked() {
                        self.save_cfg(|c| {
                            for v in vms.iter().filter(|v| !v.legacy.is_empty()) {
                                c.legacy_approved.insert(v.name.clone(), v.legacy.iter().map(|x| x.0.clone()).collect());
                            }
                        });
                        self.log(i18n::text("Tüm eski dizin eşleşmeleri onaylandı"));
                        self.confirm_approve_all = false;
                    }
                    if ui.button(i18n::text("Vazgeç")).clicked() {
                        self.confirm_approve_all = false;
                    }
                });
            });
        }

        if self.confirm_exit {
            egui::Window::new(i18n::text("Aktarım sürüyor")).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).show(&ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(i18n::text("Çıkmadan önce aktarım durdurulup ilerleme kaydedilecek. Bir sonraki açılışta kaldığınız yerden devam edebilirsiniz."));
                ui.horizontal(|ui| {
                    if ui.button(i18n::text("Aktarımı durdur ve çık")).clicked() {
                        self.hub.request_shutdown();
                        self.closing = true;
                        self.confirm_exit = false;
                    }
                    if ui.button(i18n::text("Uygulamaya dön")).clicked() { self.confirm_exit = false; }
                });
            });
        }
        if self.closing {
            egui::Window::new(i18n::text("Güvenli çıkış"))
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.spinner();
                    ui.label(i18n::text("Aktarım durduruluyor, ilerleme kaydediliyor…"));
                });
        }
        self.settings_window(&ctx, act.is_some());
        self.update_window(&ctx, self.hub.busy());
        self.profiles_window(&ctx);
        self.rules_window(&ctx);
    }
}

struct NetView {
    hist: VecDeque<f32>,
    session_start: i64,
    session_bytes: u64,
    session_active_s: u64,
    stats: crate::stats::Stats,
}

fn last_backup(v: &VmView) -> i64 {
    v.remote.iter().map(|f| f.mtime).max().unwrap_or(0)
}

fn legacy_size(v: &VmView) -> u64 {
    v.legacy.iter().map(|x| x.1).sum()
}

/// seconds until the next job start (i64::MAX if unknown)
fn next_job(v: &VmView, now: i64) -> i64 {
    v.job
        .as_ref()
        .map(|j| (j.last_start - now).rem_euclid(schedule::DAY))
        .unwrap_or(i64::MAX)
}

fn vm_progress(v: &VmView, act: Option<&Activity>) -> f32 {
    if let Some(a) = act.filter(|a| a.vm == v.name && a.total > 0) {
        return pct(a.done, a.total);
    }
    if v.in_sync {
        1.0
    } else if v.remote_size == 0 || (v.committed_at.is_none() && v.local_size == 0) {
        0.0
    } else {
        (1.0 - v.est_bytes as f64 / v.remote_size as f64).clamp(0.0, 1.0) as f32
    }
}

fn status_rank(v: &VmView, act: Option<&Activity>) -> u8 {
    if act.is_some_and(|a| a.vm == v.name) {
        0
    } else if v.last_error.is_some() && !v.in_sync {
        1
    } else if v.excluded {
        5
    } else if v.in_sync {
        4
    } else if v.busy {
        2
    } else {
        3
    }
}

fn mbs(b: f64) -> String {
    if b >= 1e6 {
        format!("{:.1} MB/s", b / 1e6)
    } else if b > 0.0 {
        format!("{:.0} KB/s", b / 1e3)
    } else {
        "0 MB/s".into()
    }
}

fn avg(it: impl Iterator<Item = f32>) -> f64 {
    let (mut s, mut n) = (0f64, 0usize);
    for x in it {
        s += x as f64;
        n += 1;
    }
    if n == 0 { 0.0 } else { s / n as f64 }
}

fn nice_ceil(x: f64) -> f64 {
    if x <= 0.0 {
        return 1.0;
    }
    let p = 10f64.powf(x.log10().floor());
    for m in [1.0, 2.0, 2.5, 5.0, 10.0] {
        if m * p >= x {
            return m * p;
        }
    }
    10.0 * p
}

fn card(ui: &mut egui::Ui, w: f32, title: &str, value: String, sub: String, color: Color32) {
    egui::Frame::group(ui.style())
        .fill(ui.visuals().faint_bg_color)
        .show(ui, |ui| {
            ui.set_width(w);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.vertical(|ui| {
                ui.label(RichText::new(title).size(11.5).weak());
                ui.label(RichText::new(value).size(22.0).strong().color(color));
                ui.label(RichText::new(&sub).size(11.0).weak())
                    .on_hover_text(sub);
            });
        });
}

fn alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

/// bytes/s per second, last `window` seconds as a bar+line chart
fn speed_chart(ui: &mut egui::Ui, hist: &VecDeque<f32>, window: usize, size: egui::Vec2) {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    let data: Vec<f32> = hist
        .iter()
        .skip(hist.len().saturating_sub(window))
        .copied()
        .collect();
    let offset = window - data.len();
    let buckets = ((rect.width() / 3.0) as usize).clamp(10, window);
    let per = window as f64 / buckets as f64;
    let vals: Vec<f64> = (0..buckets)
        .map(|b| {
            let s = (b as f64 * per) as usize;
            let e = (((b + 1) as f64 * per) as usize).clamp(s + 1, window);
            avg((s..e).map(|i| if i >= offset { data[i - offset] } else { 0.0 }))
        })
        .collect();
    let top = nice_ceil(vals.iter().cloned().fold(0.0, f64::max).max(1e6) * 1.1 / 1e6) * 1e6;
    let weak = ui.visuals().weak_text_color();
    let font = egui::FontId::proportional(10.5);
    for k in 1..=3 {
        let y = rect.bottom() - rect.height() * k as f32 / 4.0;
        p.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(0.5, alpha(weak, 60)),
        );
        p.text(
            pos2(rect.left() + 3.0, y - 1.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{:.0} MB/s", top * k as f64 / 4.0 / 1e6),
            font.clone(),
            weak,
        );
    }
    let bw = rect.width() / buckets as f32;
    let y_of = |v: f64| rect.bottom() - (v / top) as f32 * rect.height();
    for (b, v) in vals.iter().enumerate() {
        if *v > 0.0 {
            let x = rect.left() + b as f32 * bw;
            p.rect_filled(
                Rect::from_min_max(pos2(x, y_of(*v)), pos2(x + bw, rect.bottom())),
                0.0,
                alpha(BLUE, 70),
            );
        }
    }
    let pts: Vec<egui::Pos2> = vals
        .iter()
        .enumerate()
        .map(|(b, v)| pos2(rect.left() + (b as f32 + 0.5) * bw, y_of(*v)))
        .collect();
    p.add(Shape::line(pts, Stroke::new(1.6, BLUE)));
    let label = if window >= 3600 { "-1 sa" } else { "-10 dk" };
    p.text(
        rect.left_bottom() + vec2(3.0, -2.0),
        egui::Align2::LEFT_BOTTOM,
        label,
        font.clone(),
        weak,
    );
    p.text(
        rect.right_bottom() + vec2(-3.0, -2.0),
        egui::Align2::RIGHT_BOTTOM,
        i18n::text("şimdi"),
        font.clone(),
        weak,
    );
    if let Some(pos) = resp.hover_pos() {
        let b = (((pos.x - rect.left()) / bw).max(0.0) as usize).min(buckets - 1);
        let ago = ((buckets - b) as f64 * per) as i64;
        p.line_segment(
            [pos2(pos.x, rect.top()), pos2(pos.x, rect.bottom())],
            Stroke::new(1.0, alpha(weak, 120)),
        );
        let t = if ago >= 60 {
            i18n::choose(
                format!("{} dk {} sn önce", ago / 60, ago % 60),
                format!("{} min {} sec ago", ago / 60, ago % 60),
            )
        } else {
            i18n::choose(format!("{ago} sn önce"), format!("{ago} seconds ago"))
        };
        p.text(
            rect.right_top() + vec2(-4.0, 3.0),
            egui::Align2::RIGHT_TOP,
            format!("{t}: {}", mbs(vals[b])),
            egui::FontId::proportional(12.0),
            ui.visuals().strong_text_color(),
        );
    }
}

/// daily download totals, last `days` days
fn daily_chart(ui: &mut egui::Ui, stats: &crate::stats::Stats, days: i64, size: egui::Vec2) {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    let today = Local::now().date_naive();
    let vals: Vec<(chrono::NaiveDate, u64)> = (0..days)
        .rev()
        .map(|i| {
            let d = today - chrono::Duration::days(i);
            (
                d,
                stats
                    .daily
                    .get(&d.format("%Y-%m-%d").to_string())
                    .copied()
                    .unwrap_or(0),
            )
        })
        .collect();
    let max = vals.iter().map(|x| x.1).max().unwrap_or(0).max(1) as f64;
    let weak = ui.visuals().weak_text_color();
    let font = egui::FontId::proportional(10.0);
    let label_h = 13.0;
    let plot_h = rect.height() - label_h - 16.0;
    let bw = rect.width() / days as f32;
    let hover = resp
        .hover_pos()
        .map(|h| ((h.x - rect.left()).max(0.0) / bw) as usize);
    for (i, (d, v)) in vals.iter().enumerate() {
        let x = rect.left() + i as f32 * bw;
        let h = (*v as f64 / max) as f32 * plot_h;
        let bottom = rect.bottom() - label_h;
        let c = if hover == Some(i) {
            GREEN
        } else if i + 1 == vals.len() {
            BLUE
        } else {
            alpha(BLUE, 150)
        };
        if *v > 0 {
            p.rect_filled(
                Rect::from_min_max(
                    pos2(x + 2.0, bottom - h.max(2.0)),
                    pos2(x + bw - 2.0, bottom),
                ),
                2.0,
                c,
            );
        }
        p.text(
            pos2(x + bw / 2.0, rect.bottom() - 1.0),
            egui::Align2::CENTER_BOTTOM,
            d.format("%d").to_string(),
            font.clone(),
            weak,
        );
    }
    let title = match hover.and_then(|i| vals.get(i)) {
        Some((d, v)) => format!("{}: {}", d.format("%d.%m"), human(*v)),
        None => i18n::choose(
            format!("bugün {}", human(vals.last().map(|x| x.1).unwrap_or(0))),
            format!("today {}", human(vals.last().map(|x| x.1).unwrap_or(0))),
        ),
    };
    p.text(
        rect.left_top() + vec2(4.0, 2.0),
        egui::Align2::LEFT_TOP,
        title,
        egui::FontId::proportional(11.5),
        ui.visuals().text_color(),
    );
}

/// Synced / partly local / remaining as one stacked bar
fn coverage_bar(ui: &mut egui::Ui, vms: &[VmView], act: Option<&Activity>) {
    let live: Vec<&VmView> = vms.iter().filter(|v| !v.excluded).collect();
    let total: u64 = live.iter().map(|v| v.remote_size).sum::<u64>().max(1);
    let synced: u64 = live
        .iter()
        .filter(|v| v.in_sync)
        .map(|v| v.remote_size)
        .sum();
    let partial: u64 = live
        .iter()
        .filter(|v| !v.in_sync)
        .map(|v| (v.remote_size as f64 * vm_progress(v, act) as f64) as u64)
        .sum();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, alpha(AMBER, 45));
    let w1 = rect.width() * synced as f32 / total as f32;
    let w2 = rect.width() * partial as f32 / total as f32;
    p.rect_filled(
        Rect::from_min_size(rect.min, vec2(w1, rect.height())),
        4.0,
        alpha(GREEN, 190),
    );
    p.rect_filled(
        Rect::from_min_size(rect.min + vec2(w1, 0.0), vec2(w2, rect.height())),
        0.0,
        alpha(BLUE, 150),
    );
    let n_sync = live.iter().filter(|v| v.in_sync).count();
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        i18n::choose(
            format!(
                "Yerel kopya: {} / {} güncel ({:.0}%) · {} / {} yedek tamam",
                human(synced),
                human(total),
                synced as f64 * 100.0 / total as f64,
                n_sync,
                live.len()
            ),
            format!(
                "Local copy: {} / {} current ({:.0}%) · {} / {} backups complete",
                human(synced),
                human(total),
                synced as f64 * 100.0 / total as f64,
                n_sync,
                live.len()
            ),
        ),
        egui::FontId::proportional(12.5),
        ui.visuals().strong_text_color(),
    );
    resp.on_hover_text(i18n::choose(format!(
        "Yeşil: tam güncel ({})\nMavi: kısmen yerelde / indiriliyor (~{})\nTuruncu: indirilecek (~{})",
        human(synced),
        human(partial),
        human(total.saturating_sub(synced + partial))
    ), format!(
        "Green: fully up to date ({})\nBlue: partially local / downloading (~{})\nAmber: pending download (~{})",
        human(synced),
        human(partial),
        human(total.saturating_sub(synced + partial))
    )));
}

/// 24 h strip (now-3h .. now+21h) with every job's avoid window
fn timeline(ui: &mut egui::Ui, vms: &[VmView], cfg: &Config, act: Option<&Activity>) {
    let now = chrono::Utc::now().timestamp();
    let (t0, t1) = (now - 3 * 3600, now + 21 * 3600);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::hover());
    let p = ui.painter_at(rect);
    let strip = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + 26.0));
    p.rect_filled(strip, 4.0, alpha(GREEN, 35));
    let x = |t: i64| strip.left() + (t - t0) as f32 / (t1 - t0) as f32 * strip.width();
    let (before, after) = (cfg.margin_before_min * 60, cfg.margin_after_min * 60);
    // (margin start, job start, job end, margin end, name)
    let mut wins: Vec<(i64, i64, i64, i64, &str)> = Vec::new();
    for v in vms.iter().filter(|v| !v.excluded) {
        let Some(j) = &v.job else { continue };
        let k0 = (t0 - j.last_start).div_euclid(schedule::DAY);
        for k in k0 - 1..=k0 + 2 {
            let s = j.last_start + k * schedule::DAY;
            let w = (
                s - before,
                s,
                s + j.max_dur,
                s + j.max_dur + after,
                v.name.as_str(),
            );
            if w.3 > t0 && w.0 < t1 {
                wins.push(w);
            }
        }
    }
    for w in &wins {
        p.rect_filled(
            Rect::from_min_max(
                pos2(x(w.0), strip.top() + 4.0),
                pos2(x(w.3), strip.bottom() - 4.0),
            ),
            0.0,
            alpha(AMBER, 55),
        );
    }
    for w in &wins {
        let active = act.is_some_and(|a| a.vm == w.4);
        let r = Rect::from_min_max(
            pos2(x(w.1), strip.top()),
            pos2(x(w.2).max(x(w.1) + 2.0), strip.bottom()),
        );
        p.rect_filled(r, 0.0, if active { BLUE } else { alpha(RED, 200) });
    }
    let weak = ui.visuals().weak_text_color();
    let mut t = t0 - t0.rem_euclid(3600) + 3600;
    while t < t1 {
        let h = Local
            .timestamp_opt(t, 0)
            .single()
            .map(|d| d.hour())
            .unwrap_or(0);
        let xx = x(t);
        let major = h % 3 == 0;
        p.line_segment(
            [
                pos2(xx, strip.bottom()),
                pos2(xx, strip.bottom() + if major { 5.0 } else { 2.0 }),
            ],
            Stroke::new(1.0, weak),
        );
        if major {
            p.text(
                pos2(xx, rect.bottom()),
                egui::Align2::CENTER_BOTTOM,
                format!("{h:02}:00"),
                egui::FontId::proportional(10.5),
                weak,
            );
        }
        t += 3600;
    }
    let xn = x(now);
    p.line_segment(
        [pos2(xn, strip.top() - 1.0), pos2(xn, strip.bottom() + 1.0)],
        Stroke::new(2.0, Color32::WHITE),
    );
    if let Some(pos) = resp.hover_pos() {
        let th = t0 + ((pos.x - strip.left()) / strip.width() * (t1 - t0) as f32) as i64;
        let mut names: Vec<&str> = wins
            .iter()
            .filter(|w| w.0 <= th && th < w.3)
            .map(|w| w.4)
            .collect();
        names.sort();
        names.dedup();
        let txt = if names.is_empty() {
            format!("{} — serbest", schedule::hm(th))
        } else {
            format!(
                "{} — bu dizinlere dokunulmaz:\n{}",
                schedule::hm(th),
                names.join("\n")
            )
        };
        p.line_segment(
            [pos2(pos.x, strip.top()), pos2(pos.x, strip.bottom())],
            Stroke::new(1.0, Color32::WHITE),
        );
        resp.on_hover_text(txt);
    }
}

impl App {
    fn dashboard(
        &mut self,
        ui: &mut egui::Ui,
        net: &NetView,
        vms: &[VmView],
        act: Option<&Activity>,
        cfg: &Config,
        speed_est: f64,
    ) {
        let now = chrono::Utc::now().timestamp();
        let h = &net.hist;
        let inst = avg(h.iter().rev().take(3).copied());
        let avg5 = avg(h.iter().rev().take(300).copied().filter(|x| *x > 0.0));
        let peak = h.iter().copied().fold(0f32, f32::max) as f64;
        let mut remaining: u64 = vms
            .iter()
            .filter(|v| !v.in_sync && !v.excluded && act.is_none_or(|a| a.vm != v.name))
            .map(|v| v.est_bytes)
            .sum();
        if let Some(a) = act {
            remaining += a.total.saturating_sub(a.done);
        }
        let waiting_n = vms.iter().filter(|v| !v.in_sync && !v.excluded).count();
        let eta_speed = if avg5 > 0.0 { avg5 } else { speed_est };

        let strong = ui.visuals().strong_text_color();
        let weak = ui.visuals().weak_text_color();
        let compact = ui.available_width() < 1100.0 || ui.ctx().content_rect().height() < 760.0;
        let has_backups = vms.iter().any(|v| !v.excluded);
        let (eta_title, eta_value, eta_sub, eta_color) = if !has_backups {
            (
                i18n::text("Yedek durumu"),
                i18n::text("Bekleniyor").into(),
                i18n::text("Tarama ve bağlantıyı kontrol edin").into(),
                weak,
            )
        } else if remaining == 0 {
            (
                i18n::text("Yedek durumu"),
                i18n::text("Hepsi güncel").into(),
                i18n::choose(
                    format!(
                        "{} yedek korunuyor",
                        vms.iter().filter(|v| !v.excluded).count()
                    ),
                    format!(
                        "{} backups protected",
                        vms.iter().filter(|v| !v.excluded).count()
                    ),
                ),
                GREEN,
            )
        } else if !self.ctl.running.load(Ordering::SeqCst) && act.is_none() {
            (
                i18n::text("Otomatik senkron"),
                i18n::text("Duraklatıldı").into(),
                i18n::text("Devam etmek için Başlat").into(),
                AMBER,
            )
        } else {
            (
                i18n::text("Aktarım tahmini"),
                dur_h((remaining as f64 / eta_speed.max(1.0)) as i64),
                i18n::choose(
                    format!("{} ile · bekleme süreleri hariç", mbs(eta_speed)),
                    format!("at {} · excludes waiting time", mbs(eta_speed)),
                ),
                strong,
            )
        };
        let cards = [
            (
                i18n::text("Anlık hız"),
                mbs(inst),
                format!("{:.0} Mbit/s", inst * 8.0 / 1e6),
                if inst > 0.0 { BLUE } else { weak },
            ),
            (
                i18n::text("Son 5 dk · aktif ort."),
                mbs(avg5),
                i18n::choose(
                    format!("tepe (1 sa): {}", mbs(peak)),
                    format!("peak (1 h): {}", mbs(peak)),
                ),
                BLUE,
            ),
            (
                i18n::text("Bu oturum"),
                human(net.session_bytes),
                i18n::choose(
                    format!(
                        "açık {} · aktif {}",
                        dur_h(now - net.session_start),
                        dur_h(net.session_active_s as i64)
                    ),
                    format!(
                        "open {} · active {}",
                        dur_h(now - net.session_start),
                        dur_h(net.session_active_s as i64)
                    ),
                ),
                strong,
            ),
            (
                i18n::text("Toplam indirilen"),
                human(net.stats.total_bytes),
                i18n::choose(
                    format!("{} aktif", dur_h(net.stats.active_secs as i64)),
                    format!("{} active", dur_h(net.stats.active_secs as i64)),
                ),
                strong,
            ),
            (
                i18n::text("Kalan (tahmini)"),
                if has_backups {
                    human(remaining)
                } else {
                    "—".into()
                },
                format!("{waiting_n} yedek bekliyor"),
                if remaining > 0 { AMBER } else { weak },
            ),
            (eta_title, eta_value, eta_sub, eta_color),
        ];
        let indices: &[usize] = if compact {
            &[0, 4, 5]
        } else {
            &[0, 1, 2, 3, 4, 5]
        };
        ui.columns(indices.len(), |columns| {
            for (col, &i) in columns.iter_mut().zip(indices) {
                let (title, value, sub, color) = &cards[i];
                card(
                    col,
                    (col.available_width() - 18.0).max(1.0),
                    title,
                    value.clone(),
                    sub.clone(),
                    *color,
                );
            }
        });
        if !compact {
            ui.add_space(4.0);
            ui.columns(2, |columns| {
                let left = &mut columns[0];
                left.horizontal(|ui| {
                    ui.label(RichText::new(i18n::text("İndirme hızı")).size(11.5).weak());
                    ui.selectable_value(&mut self.chart_window, 600, "10 dk");
                    ui.selectable_value(&mut self.chart_window, 3600, "1 sa");
                });
                speed_chart(
                    left,
                    h,
                    self.chart_window,
                    vec2(left.available_width(), 75.0),
                );
                let right = &mut columns[1];
                right.label(
                    RichText::new(i18n::text("Günlük indirme · son 14 gün"))
                        .size(11.5)
                        .weak(),
                );
                right.add_space(22.0);
                daily_chart(right, &net.stats, 14, vec2(right.available_width(), 75.0));
            });
        }
        ui.add_space(4.0);
        coverage_bar(ui, vms, act);
        if !compact && cfg.kind == crate::rules::ProfileKind::Veeam {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(i18n::text("Önümüzdeki 24 saat:"))
                        .size(11.5)
                        .weak(),
                );
                ui.label(RichText::new("■ job").size(11.5).color(RED));
                ui.label(
                    RichText::new(i18n::text("■ bekleme payı"))
                        .size(11.5)
                        .color(AMBER),
                );
                ui.label(
                    RichText::new(i18n::text("■ serbest"))
                        .size(11.5)
                        .color(GREEN),
                );
                ui.label(
                    RichText::new(i18n::text("■ şu an indirilen"))
                        .size(11.5)
                        .color(BLUE),
                );
                ui.label(RichText::new(i18n::text("│ şimdi")).size(11.5));
            });
            timeline(ui, vms, cfg, act);
        }
    }
}

impl App {
    fn detail(&mut self, ui: &mut egui::Ui, v: &VmView, cfg: &Config, active: bool, now: i64) {
        ui.heading(&v.name);
        ui.horizontal(|ui| {
            if !active
                && ui
                    .button(i18n::text("⇣ Şimdi senkronla"))
                    .on_hover_text(i18n::text("Plan, saat sınırı, hata beklemesi ve 'durduruldu' yok sayılır. Sunucuda job çalışıyorsa ya da job penceresindeyse pencere bitince başlar. Güncelse hiçbir şey indirmez."))
                    .clicked()
            {
                self.ctl.force(&v.name);
                self.log(i18n::choose(format!("{}: elle başlatıldı", v.name), format!("{}: started manually", v.name)));
            }
            let excl = cfg.excluded.contains(&v.name);
            if ui.button(if excl { i18n::text("Dahil et") } else { i18n::text("Hariç tut") }).clicked() {
                let n = v.name.clone();
                self.save_cfg(|c| {
                    if excl {
                        c.excluded.retain(|x| *x != n);
                    } else {
                        c.excluded.push(n);
                    }
                });
            }
            if ui.button(i18n::text("📂 Klasörü aç")).clicked()
                && let Ok(path) = crate::rules::target(std::path::Path::new(&cfg.local_root), &v.name) {
                    let folder = if cfg.kind == ProfileKind::Files { path.parent().unwrap_or(&path) } else { &path };
                    let _ = std::process::Command::new("explorer").arg(folder).spawn();
            }
        });
        if let Some(j) = &v.job {
            ui.label(i18n::choose(format!(
                "Veeam job: her gün ~{} (yerel saat), süre ≤ {} — bu aralığın {} dk öncesi / {} dk sonrası beklenir",
                j.describe(),
                dur_h(j.max_dur),
                cfg.margin_before_min,
                cfg.margin_after_min
            ), format!(
                "Veeam job: daily ~{} (local time), duration ≤ {} — wait {} min before / {} min after this window",
                j.describe(),
                dur_h(j.max_dur),
                cfg.margin_before_min,
                cfg.margin_after_min
            )));
        }
        if v.incomplete && !v.in_sync {
            ui.colored_label(AMBER, i18n::text("⚠ Yerinde güncelleme sürüyor: .vbk yerel setten alınıp güncelleniyor; tamamlanınca set yeniden tutarlı olur. Plan ne olursa olsun önce bu iş bitirilir."));
        }
        if let Some(e) = &v.last_error {
            ui.colored_label(
                RED,
                i18n::choose(
                    format!("Son hata: {}", i18n::display(e)),
                    format!("Last error: {}", i18n::display(e)),
                ),
            );
        }
        if let Some(u) = v.retry_at {
            ui.colored_label(
                AMBER,
                i18n::choose(
                    format!("Otomatik yeniden deneme: {}", fmt_time(u)),
                    format!("Automatic retry: {}", fmt_time(u)),
                ),
            );
        }

        // plan
        ui.separator();
        ui.strong(i18n::text("Senkron planı"));
        let mut custom = cfg.vm_plan.contains_key(&v.name);
        if ui
            .checkbox(&mut custom, i18n::text("Bu yedek için ayrı plan kullan"))
            .changed()
        {
            let n = v.name.clone();
            self.save_cfg(|c| {
                if custom {
                    let p = c.plan;
                    c.vm_plan.insert(n, p);
                } else {
                    c.vm_plan.remove(&n);
                }
            });
        }
        if let Some(mut p) = cfg.vm_plan.get(&v.name).copied() {
            if plan_editor(ui, &format!("plan-{}", v.name), &mut p) {
                let n = v.name.clone();
                self.save_cfg(|c| {
                    c.vm_plan.insert(n, p);
                });
            }
        } else {
            ui.label(
                RichText::new(i18n::choose(
                    format!("Genel plan: {}", cfg.plan.label()),
                    format!("Default plan: {}", cfg.plan.label()),
                ))
                .weak(),
            );
        }
        let (_, txt, c, _) = next_info(v, now, active, ui.visuals().weak_text_color());
        ui.horizontal(|ui| {
            ui.label(i18n::text("Sonraki senkron:"));
            ui.label(RichText::new(txt).color(c));
        });
        ui.separator();

        // file comparison
        ui.strong(i18n::text("Dosyalar: sunucu ↔ yerel"));
        let mut names: Vec<String> = v.remote.iter().map(|f| f.name.clone()).collect();
        for l in &v.local {
            if !names.contains(&l.name) {
                names.push(l.name.clone());
            }
        }
        names.sort_by_key(|n| (crate::schedule::name_time(n).unwrap_or(i64::MAX), n.clone()));
        egui::Grid::new("files")
            .striped(true)
            .num_columns(5)
            .show(ui, |ui| {
                ui.strong(i18n::text("Dosya"));
                ui.strong(i18n::text("Sunucu"));
                ui.strong(i18n::text("Yerel"));
                ui.strong(i18n::text("Tarih (sunucu)"));
                ui.strong("");
                ui.end_row();
                for n in &names {
                    let r = v.remote.iter().find(|f| &f.name == n);
                    let l = v.local.iter().find(|f| &f.name == n);
                    let st = v.staging_files.iter().find(|f| &f.name == n);
                    let same = r.is_some_and(|r| {
                        v.committed
                            .iter()
                            .any(|c| c.name == r.name && c.size == r.size && c.mtime == r.mtime)
                            && l.is_some_and(|l| l.size == r.size)
                    });
                    ui.label(RichText::new(n).monospace().size(12.0));
                    ui.label(r.map(|f| human(f.size)).unwrap_or_else(|| "—".into()));
                    ui.label(l.map(|f| human(f.size)).unwrap_or_else(|| "—".into()));
                    ui.label(r.map(|f| fmt_time(f.mtime)).unwrap_or_default());
                    if same {
                        ui.colored_label(GREEN, i18n::text("✓ aynı"));
                    } else if r.is_none() {
                        ui.colored_label(AMBER, i18n::text("eski (silinecek)"));
                    } else if st.is_some() {
                        ui.colored_label(BLUE, i18n::text("⇣ hazırlanıyor"));
                    } else {
                        ui.colored_label(AMBER, i18n::text("indirilecek"));
                    }
                    ui.end_row();
                }
            });

        ui.separator();
        if cfg.kind == crate::rules::ProfileKind::Files {
            return;
        }
        ui.strong(i18n::text(
            "Eski yerel dizinler (aynı sunucu, eski isimlendirme)",
        ));
        if v.legacy.is_empty() {
            ui.label(i18n::text("Yok."));
        } else {
            ui.label(RichText::new(i18n::text("İşaretlenenler, bu yedeğin yeni seti başarıyla indirildikten sonra otomatik silinir. Disk yetmezse indirme öncesi silinir (ayarlardan kapatılabilir).")).weak());
            let ap = cfg
                .legacy_approved
                .get(&v.name)
                .cloned()
                .unwrap_or_default();
            for (n, s) in &v.legacy {
                let mut on = ap.contains(n);
                if ui
                    .checkbox(&mut on, format!("{n}  ({})", human(*s)))
                    .changed()
                {
                    let (vn, nn) = (v.name.clone(), n.clone());
                    self.save_cfg(|c| {
                        let e = c.legacy_approved.entry(vn).or_default();
                        e.retain(|x| *x != nn);
                        if on {
                            e.push(nn);
                        }
                    });
                }
            }
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context, busy: bool) {
        let Some(d) = self.settings.as_mut() else {
            return;
        };
        let mut open = true;
        let (mut save, mut cancel) = (false, false);
        let section = |ui: &mut egui::Ui, t: &str| {
            ui.add_space(6.0);
            ui.label(RichText::new(t).strong().color(BLUE));
            ui.separator();
        };
        egui::Window::new(i18n::text("Ayarlar")).open(&mut open).collapsible(false).default_width(560.0).default_height(620.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(ctx.content_rect().height() - 200.0).show(ui, |ui| {
                let c = &mut d.cfg;
                ui.horizontal(|ui| {
                    ui.label("Dil / Language");
                    ui.selectable_value(&mut c.language, crate::i18n::Language::Turkish, "Türkçe");
                    ui.selectable_value(&mut c.language, crate::i18n::Language::English, "English");
                });
                section(ui, i18n::text("Bağlantı"));
                egui::Grid::new("set-conn").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(i18n::text("Sunucu"));
                    ui.text_edit_singleline(&mut c.host);
                    ui.end_row();
                    ui.label("Port");
                    ui.add(egui::DragValue::new(&mut c.port).range(1..=65535));
                    ui.end_row();
                    ui.label(i18n::text("Kullanıcı"));
                    ui.text_edit_singleline(&mut c.user);
                    ui.end_row();
                    ui.label(i18n::text("Parola"));
                    ui.add(egui::TextEdit::singleline(&mut d.pw).password(true).hint_text(if c.password_enc.is_empty() { i18n::text("girilmemiş") } else { i18n::text("kayıtlı (değiştirmek için yazın)") }));
                    ui.end_row();
                    ui.label(i18n::text("Sunucu anahtarı"));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(if c.host_fingerprint.is_empty() { i18n::text("(ilk bağlantıda kaydedilecek)") } else { &c.host_fingerprint }).monospace().size(11.0));
                        if !c.host_fingerprint.is_empty() && ui.small_button(i18n::text("Sıfırla")).on_hover_text(i18n::text("Sadece sunucu yeniden kurulduysa: bir sonraki bağlantıda yeni anahtar kabul edilir")).clicked() {
                            c.host_fingerprint.clear();
                        }
                    });
                    ui.end_row();
                    ui.label(i18n::text("Sunucu yedek dizini"));
                    ui.text_edit_singleline(&mut c.remote_root);
                    ui.end_row();
                    ui.label(i18n::text("Yerel yedek dizini"));
                    ui.text_edit_singleline(&mut c.local_root);
                    ui.end_row();
                });

                section(ui, i18n::text("Zamanlama — yedekler ne sıklıkla indirilsin?"));
                ui.horizontal(|ui| {
                    ui.label(i18n::text("Genel plan:"));
                    plan_editor(ui, "plan-global", &mut c.plan);
                });
                if !matches!(c.plan, Plan::Continuous | Plan::Manual) {
                    ui.horizontal(|ui| {
                        ui.label(i18n::text("Planlanan günde en erken"));
                        ui.add(egui::DragValue::new(&mut c.plan_hour).range(0..=23).suffix(":00"));
                        ui.label(i18n::text("'dan sonra başla"));
                    });
                }
                ui.label(
                    RichText::new(match c.plan {
                        Plan::Continuous => i18n::text("Sunucuda yeni yedek çıktıkça (job penceresi bitince) indirilir. Veeam günlük çalıştığı için pratikte her gün bir kez.").to_string(),
                        Plan::EveryDays(1) => i18n::text("Her gün bir kez indirilir; aradaki tüm restore point'ler de gelir.").to_string(),
                        Plan::EveryDays(n) => i18n::choose(format!("Son başarılı senkrondan {n} gün sonra yeniden indirilir. Aradaki restore point'lerin hepsi de gelir; sadece daha seyrek, toplu yapılır (sunucu ve USB disk daha az yorulur)."), format!("Download again {n} days after the last successful sync, including all intermediate restore points. Less frequent batches reduce server and USB disk activity.")),
                        Plan::Weekdays(_) => i18n::text("Seçili günlerde bir kez indirilir. O gün kaçarsa (bilgisayar kapalı vb.) ilk fırsatta yapılır.").to_string(),
                        Plan::Manual => i18n::text("Hiçbir yedek kendiliğinden indirilmez; ayrıntı panelinden 'Şimdi senkronla' ile başlatılır.").to_string(),
                    })
                    .weak()
                    .size(12.0),
                );
                ui.label(RichText::new(i18n::text("Hiç indirilmemiş yedekler ve yarım kalmış yerinde güncellemeler plandan bağımsız hemen tamamlanır. Ayrıntı panelinden her yedeğe ayrı plan verilebilir.")).weak().size(12.0));
                egui::Grid::new("set-time").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(i18n::text("Saat sınırı"));
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut c.hours_limit, i18n::text("sadece"));
                        ui.add_enabled(c.hours_limit, egui::DragValue::new(&mut c.hours_start).range(0..=23).suffix(":00"));
                        ui.label("–");
                        ui.add_enabled(c.hours_limit, egui::DragValue::new(&mut c.hours_end).range(0..=23).suffix(":00"));
                        ui.label(i18n::text("arası indir"));
                    });
                    ui.end_row();
                    ui.label(i18n::text("Job öncesi bekleme (dk)"));
                    ui.add(egui::DragValue::new(&mut c.margin_before_min).range(0..=240));
                    ui.end_row();
                    ui.label(i18n::text("Job sonrası bekleme (dk)"));
                    ui.add(egui::DragValue::new(&mut c.margin_after_min).range(0..=240));
                    ui.end_row();
                    ui.label(i18n::text("Tarama aralığı (dk)"));
                    ui.add(egui::DragValue::new(&mut c.scan_interval_min).range(1..=240));
                    ui.end_row();
                });

                section(ui, i18n::text("Aktarım"));
                egui::Grid::new("set-xfer").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(i18n::text("Hız sınırı"));
                    ui.add(egui::DragValue::new(&mut c.max_mbps).range(0.0..=10000.0).suffix(" Mbit/s")).on_hover_text(i18n::text("0 = sınırsız"));
                    ui.end_row();
                    ui.label(i18n::text("Güncelleme modu"));
                    ui.vertical(|ui| {
                        ui.add_enabled_ui(c.kind == crate::rules::ProfileKind::Veeam, |ui| { ui.radio_value(&mut c.mode, UpdateMode::InPlace, i18n::text("Yerinde: .vbk'de sadece değişen bloklar yazılır (hızlı, USB diski yormaz)")); });
                        ui.radio_value(&mut c.mode, UpdateMode::Safe, i18n::text("Güvenli: yeni set tam kopya olarak hazırlanır, eski set sona kadar sağlam kalır (2× yer, çok yazma)"));
                    });
                    ui.end_row();
                });

                section(ui, "Disk");
                egui::Grid::new("set-disk").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(i18n::text("Min. boş alan"));
                    ui.add(egui::DragValue::new(&mut c.min_free_gb).range(0..=2000).suffix(" GB"));
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut c.free_space_first, i18n::text("Yer yetmezse onaylı eski dizinleri indirmeden önce sil"));
                    ui.end_row();
                });

                section(ui, i18n::text("Başlangıç"));
                ui.checkbox(&mut c.auto_start, i18n::text("Program açılınca otomatik senkronu başlat"));
                ui.checkbox(&mut d.autostart, i18n::text("Windows açılınca Oh My Backup'ı başlat"));
            });
            ui.separator();
            if let Some(e) = &d.err {
                ui.colored_label(RED, e);
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.preview, egui::Button::new(i18n::text("💾 Kaydet"))).on_hover_text(if self.preview { i18n::text("Önizlemede ayarlar kaydedilmez") } else { i18n::text("Ayarları uygula ve kaydet") }).clicked() {
                    save = true;
                }
                if ui.button(i18n::text("Vazgeç")).clicked() {
                    cancel = true;
                }
            });
        });
        if save {
            if let Err(e) = validate(&d.cfg) {
                d.err = Some(e);
                return;
            }
            let mut newc = d.cfg.clone();
            if !d.pw.is_empty() {
                newc.set_password(&d.pw);
            }
            let old = self.ctl.config.lock().unwrap().clone();
            newc.github_token_enc = old.github_token_enc.clone();
            let reconnect = old.host != newc.host
                || old.port != newc.port
                || old.user != newc.user
                || !d.pw.is_empty()
                || old.host_fingerprint != newc.host_fingerprint;
            let roots_changed =
                old.local_root != newc.local_root || old.remote_root != newc.remote_root;
            if roots_changed
                && let Err(error) = self
                    .hub
                    .validate_target(self.hub.selected, &newc.local_root)
            {
                d.err = Some(error.to_string());
                return;
            }
            if let Err(e) = newc.save() {
                d.err = Some(i18n::choose(
                    format!("Kaydedilemedi: {e:#}"),
                    format!("Could not save: {e:#}"),
                ));
                return;
            }
            if d.autostart != d.autostart_orig
                && let Err(e) = set_autostart(d.autostart)
            {
                self.notice = Some(i18n::choose(
                    format!("Ayarlar kaydedildi; Windows başlangıcı değiştirilemedi: {e}"),
                    format!("Settings saved; Windows startup could not be changed: {e}"),
                ));
            }
            let language = newc.language;
            *self.ctl.config.lock().unwrap() = newc;
            self.save_language(language);
            if roots_changed && busy {
                // the running job works on the old paths: stop it, it continues on the new ones
                self.ctl.cancel.store(true, Ordering::SeqCst);
            }
            if reconnect {
                self.ctl.reconnect.store(true, Ordering::SeqCst);
                if busy {
                    self.ctl.cancel.store(true, Ordering::SeqCst);
                }
            }
            self.ctl.scan_now.store(true, Ordering::SeqCst);
            self.log(i18n::text("Ayarlar kaydedildi"));
            self.ctl.wake();
            self.settings = None;
        } else if cancel || !open {
            self.settings = None;
        }
    }
}

fn autostart_enabled() -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("reg")
        .args(["query", RUN_KEY, "/v", "HBSync"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

impl App {
    fn profiles_window(&mut self, ctx: &egui::Context) {
        if !self.profiles_open {
            return;
        }
        if self.settings.is_some() || self.rules_draft.is_some() {
            return;
        }
        let mut open = true;
        let mut close = false;
        let entries = self
            .hub
            .profiles
            .iter()
            .map(|p| p.entry.clone())
            .collect::<Vec<_>>();
        egui::Window::new(i18n::choose("Senkron profilleri", "Sync profiles")).open(&mut open).default_width(500.0).show(ctx, |ui| {
            ui.label(i18n::choose("Her profilin bağlantısı, hedefi ve kayıtları bağımsızdır.", "Each profile has its own connection, destination and state."));
            for (index, entry) in entries.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.strong(&entry.name);
                    ui.label(if entry.kind == crate::rules::ProfileKind::Veeam { "Veeam" } else { i18n::choose("Dosyalar", "Files") });
                    if ui.add_enabled(self.pending_archive.is_none(), egui::Button::new(i18n::choose("Seç", "Select"))).clicked() { self.select_profile(index); self.rename_profile = entry.name.clone(); }
                });
            }
            ui.separator();
            ui.label(i18n::choose("Seçili profilin adı", "Selected profile name"));
            ui.text_edit_singleline(&mut self.rename_profile);
            if ui.add_enabled(!self.preview && self.pending_archive.is_none(), egui::Button::new(i18n::choose("Adı kaydet", "Save name"))).clicked()
                && let Err(error) = self.hub.rename(self.hub.selected, &self.rename_profile) { self.notice = Some(error.to_string()); }
            if ui.add_enabled(!self.preview && self.hub.selected > 0 && self.pending_archive.is_none(), egui::Button::new(i18n::choose("Seçili profili arşivle", "Archive selected profile"))).clicked() {
                let profile = &self.hub.profiles[self.hub.selected];
                profile.ctl.shutdown.store(true, Ordering::SeqCst); profile.ctl.cancel.store(true, Ordering::SeqCst); profile.ctl.running.store(false, Ordering::SeqCst); profile.ctl.wake();
                self.pending_archive = Some(profile.entry.id.clone());
            }
            ui.label(i18n::choose("Arşivleme motoru durdurur; ayarlar ve hedef dosyalar saklanır. Taşınan ilk profil korunur.", "Archiving stops the worker and retains settings and destination files. The migrated primary profile is protected."));
            if self.pending_archive.is_some() { ui.spinner(); ui.label(i18n::choose("Profil güvenle durduruluyor…", "Stopping the profile safely…")); }
            ui.separator(); ui.strong(i18n::choose("Yeni profil", "New profile"));
            ui.text_edit_singleline(&mut self.profile_name);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.new_kind, crate::rules::ProfileKind::Files, i18n::choose("Genel dosya senkronu", "General file sync"));
                ui.selectable_value(&mut self.new_kind, crate::rules::ProfileKind::Veeam, "Veeam");
            });
            if ui.add_enabled(!self.preview && self.pending_archive.is_none() && !self.profile_name.trim().is_empty(), egui::Button::new(i18n::choose("Oluştur ve yapılandır", "Create and configure"))).clicked() {
                match self.hub.add(&self.profile_name, self.new_kind) {
                    Ok(index) => {
                        self.hub.start(index); self.select_profile(index);
                        let cfg = self.ctl.config.lock().unwrap().clone(); let a = autostart_enabled();
                        self.settings = Some(Draft { cfg, pw: String::new(), autostart: a, autostart_orig: a, err: None });
                        self.profile_name.clear(); close = true;
                    }
                    Err(error) => self.notice = Some(error.to_string()),
                }
            }
        });
        self.profiles_open = open && !close;
    }

    fn rules_window(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.rules_draft.take() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        egui::Window::new(i18n::choose("Dosya kuralları ve önizleme", "File rules and preview")).open(&mut open).default_width(680.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(ctx.content_rect().height() - 160.0).show(ui, |ui| {
                ui.checkbox(&mut draft.rules.recursive, i18n::choose("Alt klasörleri izle", "Watch subfolders"));
                ui.label(i18n::choose("Dahil et — her satıra bir desen (*, **, ?)", "Include — one pattern per line (*, **, ?)"));
                ui.add(egui::TextEdit::multiline(&mut draft.includes).desired_rows(3).desired_width(f32::INFINITY));
                ui.label(i18n::choose("Hariç tut — her satıra bir desen", "Exclude — one pattern per line"));
                ui.add(egui::TextEdit::multiline(&mut draft.excludes).desired_rows(3).desired_width(f32::INFINITY));
                ui.horizontal(|ui| { ui.label(i18n::choose("Değişmeden bekleme (sn)", "Quiet period (sec)")); ui.add(egui::DragValue::new(&mut draft.rules.quiet_seconds).range(0..=86400)); });
                ui.horizontal(|ui| { ui.label(i18n::choose("Önceki sürüm sayısı", "Previous versions to retain")); ui.add(egui::DragValue::new(&mut draft.rules.keep_versions).range(1..=100)); });
                ui.checkbox(&mut draft.rules.overwrite_unmanaged, i18n::choose("Önceden var olan kayıtsız dosyaların üzerine yaz (önceki kopya saklanır)", "Allow replacing pre-existing unmanaged files (previous copy is retained)"));
                ui.checkbox(&mut draft.rules.protect_local_changes, i18n::choose("Uygulama dışında değiştirilen yerel dosyaları koru", "Protect local files changed outside the app"));
                ui.label(i18n::choose("Kaynakta silinen dosya", "When a source file is deleted"));
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(&mut draft.rules.deletion, crate::rules::Deletion::Keep, i18n::choose("Yerelde sakla", "Keep locally"));
                    ui.selectable_value(&mut draft.rules.deletion, crate::rules::Deletion::Quarantine, i18n::choose("Karantinaya taşı", "Quarantine"));
                    if ui.selectable_value(&mut draft.rules.deletion, crate::rules::Deletion::Mirror, i18n::choose("Hedeften sil", "Delete from target")).changed() { draft.delete_confirmed = false; }
                });
                if draft.rules.deletion == crate::rules::Deletion::Mirror { ui.checkbox(&mut draft.delete_confirmed, i18n::choose("Yalnızca yönetilen dosyalara kalıcı silme uygulanmasını onaylıyorum", "I approve permanent deletion of managed files only")); }
                draft.rules.include = draft.includes.lines().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect();
                draft.rules.exclude = draft.excludes.lines().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect();
                ui.horizontal_wrapped(|ui| {
                    if ui.button(i18n::choose("Ne yapılacağını göster", "Preview actions")).clicked() {
                        let result = (|| -> anyhow::Result<Vec<crate::generic::Item>> {
                            let mut cfg = self.ctl.config.lock().unwrap().clone(); cfg.rules = draft.rules.clone();
                            cfg.rules.compile()?;
                            let snapshot = self.ctl.snapshot.lock().unwrap().clone().ok_or_else(|| anyhow::anyhow!("Scan this profile before previewing rules"))?;
                            if snapshot.context != crate::generic::source_context(&cfg) { anyhow::bail!("Source changed; scan the updated profile first"); }
                            let state = crate::generic::FileState::load(&cfg.local_root)?;
                            state.check_source(&snapshot.context)?;
                            let local = crate::generic::local_files(&cfg, &snapshot, &state)?;
                            crate::generic::plan(&cfg, &snapshot, &state, &local, chrono::Utc::now().timestamp(), None, true)
                        })();
                        match result { Ok(items) => { draft.preview = items; draft.error = None; }, Err(error) => draft.error = Some(error.to_string()) }
                    }
                    if ui.button(i18n::choose("Kuralları kopyala", "Copy rules")).clicked() && let Ok(json) = serde_json::to_string_pretty(&draft.rules) { ctx.copy_text(json); }
                    if ui.add_enabled(!self.preview && (draft.rules.deletion != crate::rules::Deletion::Mirror || draft.delete_confirmed), egui::Button::new(i18n::choose("Kuralları kaydet", "Save rules"))).clicked() { save = true; }
                });
                if let Some(error) = &draft.error { ui.colored_label(RED, error); }
                if !draft.preview.is_empty() {
                    let transfers = draft.preview.iter().filter(|i| i.action == crate::generic::Action::Copy).count();
                    let bytes = draft.preview.iter().map(|i| i.bytes).sum();
                    ui.strong(format!("{} · {} / {}", human(bytes), transfers, draft.preview.len()));
                    for item in draft.preview.iter().take(500) { ui.label(format!("{}  {} — {}", item.action.label(), item.name, i18n::display(&item.reason))); }
                    if draft.preview.len() > 500 { ui.label(i18n::choose("İlk 500 sonuç gösteriliyor.", "Showing the first 500 results.")); }
                }
                ui.separator(); ui.strong(i18n::choose("Son çalışmalar", "Recent runs"));
                let cfg = self.ctl.config.lock().unwrap().clone();
                if let Ok(state) = crate::generic::FileState::load(&cfg.local_root) {
                    for run in state.history.iter().rev().take(15) { ui.label(format!("{} · {} {} · {} {} · {}{}", fmt_time(run.at), run.copied, i18n::choose("aktarıldı", "transferred"), run.removed, i18n::choose("kaldırıldı", "removed"), human(run.bytes), run.error.as_ref().map(|s| format!(" · {s}")).unwrap_or_default())); }
                }
            });
        });
        if save {
            match draft.rules.compile() {
                Err(error) => draft.error = Some(error.to_string()),
                Ok(_) => {
                    let mut config = self.ctl.config.lock().unwrap();
                    let mut next = config.clone();
                    next.rules = draft.rules.clone();
                    match next.save() {
                        Ok(()) => {
                            *config = next;
                            drop(config);
                            self.ctl.cancel.store(true, Ordering::SeqCst);
                            self.ctl.scan_now.store(true, Ordering::SeqCst);
                            self.ctl.wake();
                            return;
                        }
                        Err(error) => draft.error = Some(error.to_string()),
                    }
                }
            }
        }
        if open {
            self.rules_draft = Some(draft);
        }
    }
}

impl App {
    fn update_window(&mut self, ctx: &egui::Context, transferring: bool) {
        if !self.update_open {
            return;
        }
        let mut open = true;
        let state = self.updater.state.clone();
        let installed = crate::update::is_installed();
        egui::Window::new(i18n::choose("Uygulama güncellemeleri", "Application updates")).open(&mut open).default_width(520.0).resizable(false).show(ctx, |ui| {
            ui.label(format!("Oh My Backup v{}", env!("CARGO_PKG_VERSION")));
            ui.label(crate::update::install_root().display().to_string());
            if !installed {
                ui.colored_label(AMBER, i18n::choose("İçeriden güncellemek için önce install.ps1 ile sabit klasöre kurun.", "Run install.ps1 to install in the fixed folder before applying updates."));
            }
            ui.label(i18n::choose("Kararlı GitHub sürümleri kontrol edilir; eski sürüme dönüş yapılmaz.", "Checks stable GitHub releases; automatic downgrades are blocked."));
            ui.separator();
            match state {
                crate::update::State::Idle => { ui.label(i18n::choose("Güncelleme henüz kontrol edilmedi.", "Updates have not been checked yet.")); }
                crate::update::State::Checking => { ui.spinner(); ui.label(i18n::choose("Sürüm kontrol ediliyor…", "Checking releases…")); }
                crate::update::State::Current(latest) => { ui.colored_label(GREEN, i18n::choose(format!("Güncelsiniz. Son yayınlanan: {latest}"), format!("You are up to date. Latest published: {latest}"))); }
                crate::update::State::Available(release) => {
                    ui.colored_label(BLUE, i18n::choose(format!("Yeni sürüm: {}", release.version), format!("New version: {}", release.version)));
                    if ui.add_enabled(!self.preview, egui::Button::new(i18n::choose("İndir ve doğrula", "Download and verify"))).clicked() {
                        let token = self.primary.config.lock().unwrap().github_token();
                        self.updater.download(release, token);
                    }
                }
                crate::update::State::Downloading => { ui.spinner(); ui.label(i18n::choose("Paket indiriliyor ve SHA-256 ile doğrulanıyor…", "Downloading and verifying the package with SHA-256…")); }
                crate::update::State::Ready(package) => {
                    ui.colored_label(GREEN, i18n::choose(format!("{} doğrulandı, kuruluma hazır.", package.version), format!("{} verified and ready to install.", package.version)));
                    if transferring { ui.colored_label(AMBER, i18n::choose("Aktarım tamamlandıktan sonra güncelleyebilirsiniz.", "You can apply the update after the transfer finishes.")); }
                    if ui.add_enabled(installed && !transferring && !self.preview && self.update_pending.is_none(), egui::Button::new(i18n::choose("Güncelle ve yeniden başlat", "Update and restart"))).clicked() {
                        self.hub.request_shutdown();
                        self.update_pending = Some(package);
                        self.updater.state = crate::update::State::Applying;
                    }
                }
                crate::update::State::Applying => { ui.spinner(); ui.label(i18n::choose("Motor güvenle durduruluyor. Uygulama güncellenip yeniden açılacak…", "Stopping the engine safely. The app will update and restart…")); }
                crate::update::State::Error(error) => { ui.colored_label(RED, error); }
            }
            if ui.add_enabled(!self.preview && !self.updater.busy() && self.update_pending.is_none(), egui::Button::new(i18n::choose("Güncellemeleri kontrol et", "Check for updates"))).clicked() {
                self.updater.check(self.primary.config.lock().unwrap().github_token());
            }
            ui.separator();
            ui.strong(i18n::choose("Private repo erişimi", "Private repository access"));
            ui.label(i18n::choose("Varsa mevcut gh oturumu veya GH_TOKEN kullanılır. Alternatif: bu repo için Contents: read yetkili token kaydedin.", "Uses an existing gh session or GH_TOKEN when available. Alternatively, save a token with Contents: read for this repository."));
            ui.add(egui::TextEdit::singleline(&mut self.update_token).password(true).hint_text(i18n::choose("GitHub token (DPAPI ile şifrelenir)", "GitHub token (encrypted with DPAPI)")));
            let mut save_token = false;
            let mut clear_token = false;
            ui.horizontal(|ui| {
                save_token = ui.add_enabled(!self.preview && !self.updater.busy() && !self.update_token.trim().is_empty(), egui::Button::new(i18n::choose("Token kaydet", "Save token"))).clicked();
                clear_token = ui.add_enabled(!self.preview && !self.updater.busy() && !self.primary.config.lock().unwrap().github_token_enc.is_empty(), egui::Button::new(i18n::choose("Kayıtlı token'ı kaldır", "Remove saved token"))).clicked();
            });
            if save_token || clear_token {
                let result = (|| -> anyhow::Result<()> {
                    let mut config = self.primary.config.lock().unwrap();
                    let mut next = config.clone();
                    next.set_github_token(if clear_token { "" } else { self.update_token.trim() })?;
                    next.save()?;
                    *config = next;
                    Ok(())
                })();
                match result {
                    Ok(()) => { self.update_token.clear(); self.updater.state = crate::update::State::Idle; }
                    Err(error) => self.updater.state = crate::update::State::Error(error.to_string()),
                }
            }
            if let Ok(bytes) = std::fs::read(crate::update::cache_root().join("last-result.json"))
                && let Ok(result) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    ui.label(format!("{}: {} {}", i18n::choose("Son güncelleme", "Last update"), result["status"].as_str().unwrap_or(""), result["version"].as_str().unwrap_or("")));
                }
        });
        self.update_open = open;
        if !open {
            self.update_token.clear();
        }
    }
}

fn set_autostart(on: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new("reg");
    if on {
        cmd.args([
            "add",
            RUN_KEY,
            "/v",
            "HBSync",
            "/t",
            "REG_SZ",
            "/f",
            "/d",
            &format!("\"{}\"", exe.display()),
        ]);
    } else {
        cmd.args(["delete", RUN_KEY, "/v", "HBSync", "/f"]);
    }
    let out = cmd
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn log_filters_recognize_both_languages() {
        assert!(is_error_line("Bağlantı hatası: reddedildi"));
        assert!(!is_error_line("Backup completed successfully"));
        assert!(is_error_line("Connection error: refused"));
        assert!(is_important_line("Connected to server"));
        assert!(is_important_line("Automatic sync started"));
    }
    #[test]
    fn preview_renders_in_both_languages_at_compact_and_desktop_sizes() {
        for language in [
            crate::i18n::Language::Turkish,
            crate::i18n::Language::English,
        ] {
            for size in [vec2(900.0, 500.0), vec2(1500.0, 900.0)] {
                let ctx = egui::Context::default();
                setup_fonts(&ctx);
                assert_eq!(ctx.theme(), egui::Theme::Dark);
                assert_eq!(
                    ctx.global_style().visuals.panel_fill,
                    Color32::from_rgb(15, 23, 34)
                );
                let ctl = Control::new(Config {
                    language,
                    local_root: std::env::temp_dir()
                        .join("oh-my-backup-render")
                        .to_string_lossy()
                        .into(),
                    ..Config::default()
                });
                crate::preview::populate(&ctl);
                let mut app = App::new(ctl, true);
                app.selected = Some("03_APPLICATION".into());
                app.update_open = true;
                app.settings = Some(Draft {
                    cfg: app.ctl.config.lock().unwrap().clone(),
                    pw: String::new(),
                    autostart: false,
                    autostart_orig: false,
                    err: None,
                });
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), size)),
                        ..Default::default()
                    },
                    |ui| app.render(ui),
                );
                assert!(!output.shapes.is_empty(), "preview must actually draw");
            }
        }
        crate::i18n::set_language(crate::i18n::Language::Turkish);
    }
    #[test]
    fn generic_dashboard_and_rules_render_in_both_languages() {
        for language in [
            crate::i18n::Language::Turkish,
            crate::i18n::Language::English,
        ] {
            for size in [vec2(900.0, 500.0), vec2(1500.0, 900.0)] {
                let ctx = egui::Context::default();
                setup_fonts(&ctx);
                let cfg = Config {
                    kind: ProfileKind::Files,
                    mode: UpdateMode::Safe,
                    language,
                    ..Default::default()
                };
                let ctl = Control::new(cfg.clone());
                crate::preview::populate_files(&ctl);
                let mut app = App::new(ctl, true);
                app.selected = Some("documents/report.pdf".into());
                app.rules_draft = Some(RulesDraft {
                    rules: cfg.rules.clone(),
                    includes: "**".into(),
                    excludes: "**/*.tmp".into(),
                    delete_confirmed: false,
                    error: None,
                    preview: Vec::new(),
                });
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), size)),
                        ..Default::default()
                    },
                    |ui| app.render(ui),
                );
                assert!(!output.shapes.is_empty());
            }
        }
    }
    #[test]
    fn settings_reject_invalid_scheduler_values() {
        let mut cfg = Config {
            host: "backup.example.invalid".into(),
            ..Default::default()
        };
        assert!(validate(&cfg).is_ok(), "unaffected control: defaults");
        cfg.scan_interval_min = 0;
        let actual = validate(&cfg);
        println!("EXPECTED: invalid scan interval rejected; ACTUAL: {actual:?}");
        assert!(
            actual.is_err(),
            "PROBLEM CONFIRMED: zero interval reaches clamp(10, 0)"
        );
        cfg.scan_interval_min = 10;
        cfg.max_mbps = f64::NAN;
        assert!(validate(&cfg).is_err());
        cfg.max_mbps = 0.0;
        cfg.hours_start = 24;
        assert!(validate(&cfg).is_err());
        println!("PROBLEM NOT REPRODUCED; FIX VERIFIED");
    }
}
