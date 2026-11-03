#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod blk;
mod config;
mod desktop;
mod generic;
mod i18n;
mod preview;
mod profiles;
mod remote;
mod rules;
mod schedule;
mod stats;
mod sync;
mod ui;
mod update;

use eframe::egui;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 && args[1] == "--version" {
        println!("Oh My Backup {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.len() == 2 && args[1] == "--check-update" {
        let cfg = config::Config::load();
        match update::check(&cfg.github_token()) {
            Ok(release) => println!(
                "Installed: {}; latest stable: {}",
                env!("CARGO_PKG_VERSION"),
                release.version
            ),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    let files_preview = args.iter().any(|arg| arg == "--preview-files");
    let preview = files_preview || args.iter().any(|arg| arg == "--preview");
    // oh-my-backup.exe --set-password <pw>   (stores the password DPAPI-encrypted, then exits)
    if args.len() == 3 && args[1] == "--set-password" {
        let mut c = config::Config::load();
        c.set_password(&args[2]);
        c.save().expect("config kaydedilemedi");
        println!(
            "{}",
            i18n::choose(
                format!("parola kaydedildi: {}", config::Config::path().display()),
                format!("password saved: {}", config::Config::path().display())
            )
        );
        return Ok(());
    }

    desktop::identify();
    let _instance = if preview {
        None
    } else {
        let Some(instance) = desktop::Instance::acquire() else {
            eprintln!(
                "{}",
                i18n::choose(
                    "Oh My Backup zaten çalışıyor; ikinci senkron motoru başlatılmadı.".to_string(),
                    "Oh My Backup is already running; no second sync engine was started."
                        .to_string()
                )
            );
            return Ok(());
        };
        Some(instance)
    };
    if args.len() == 2 && args[1] == "--once" {
        let cfg = config::Config::load();
        i18n::set_language(cfg.language);
        let ctl = sync::Control::new(cfg);
        ctl.once.store(true, std::sync::atomic::Ordering::SeqCst);
        ctl.running.store(true, std::sync::atomic::Ordering::SeqCst);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("tokio");
        rt.block_on(sync::engine(ctl));
        return Ok(());
    }

    let mut cfg = if preview {
        config::Config {
            auto_start: false,
            local_root: std::env::temp_dir()
                .join("HBSync-preview")
                .to_string_lossy()
                .into(),
            ..Default::default()
        }
    } else {
        config::Config::load()
    };
    if !preview && !config::Config::path().exists() {
        cfg.kind = rules::ProfileKind::Files;
        cfg.mode = config::UpdateMode::Safe;
        cfg.scan_interval_min = 1;
    }
    if files_preview {
        cfg.kind = rules::ProfileKind::Files;
        cfg.mode = config::UpdateMode::Safe;
    }
    i18n::set_language(cfg.language);
    if !preview && !config::Config::path().exists() {
        let _ = cfg.save();
    }
    let ctl = sync::Control::new(cfg);
    let mut profile_error = None;
    let mut hub = if preview {
        profiles::Hub::single(ctl)
    } else {
        match profiles::Hub::load() {
            Ok(hub) => hub,
            Err(error) => {
                profile_error = Some(format!(
                    "Profile catalog: {error}. Profile management is disabled."
                ));
                profiles::Hub::single(ctl)
            }
        }
    };
    if preview {
        if files_preview {
            preview::populate_files(&hub.profiles[0].ctl);
        } else {
            preview::populate(&hub.profiles[0].ctl);
        }
    } else {
        hub.start_all();
    }

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1500.0, 900.0])
            .with_min_inner_size([900.0, 500.0])
            .with_icon(desktop::icon())
            .with_title("Oh My Backup — Backup & Sync"),
        ..Default::default()
    };
    eframe::run_native(
        "Oh My Backup",
        opts,
        Box::new(move |cc| {
            ui::setup_fonts(&cc.egui_ctx);
            Ok(Box::new(ui::App::with_profiles(
                hub,
                preview,
                profile_error,
            )))
        }),
    )
}
