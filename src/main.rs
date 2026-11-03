#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod blk;
mod config;
mod desktop;
mod i18n;
mod preview;
mod remote;
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
    let preview = args.iter().any(|arg| arg == "--preview");
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

    let cfg = if preview {
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
    i18n::set_language(cfg.language);
    if !preview && !config::Config::path().exists() {
        let _ = cfg.save();
    }
    let ctl = sync::Control::new(cfg);
    if preview {
        preview::populate(&ctl);
    } else {
        sync::start_sampler(ctl.clone());
        let c2 = ctl.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .expect("tokio");
            rt.block_on(sync::engine(c2));
        });
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
            *ctl.ctx.lock().unwrap() = Some(cc.egui_ctx.clone());
            Ok(Box::new(ui::App::new(ctl, preview)))
        }),
    )
}
