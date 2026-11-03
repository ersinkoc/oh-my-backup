use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=assets/hbsync.rc");
    println!("cargo:rerun-if-changed=assets/hbsync.ico");
    println!("cargo:rerun-if-env-changed=RC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Use the Windows SDK already required by the MSVC Rust toolchain.
    let rc = env::var_os("RC")
        .map(PathBuf::from)
        .or_else(|| {
            if Command::new("rc.exe").arg("/?").output().is_ok() {
                return Some(PathBuf::from("rc.exe"));
            }
            let sdk = PathBuf::from(env::var_os("ProgramFiles(x86)")?).join("Windows Kits/10/bin");
            let host = if env::consts::ARCH == "aarch64" {
                "arm64"
            } else {
                "x64"
            };
            let mut versions: Vec<_> = std::fs::read_dir(sdk)
                .ok()?
                .flatten()
                .map(|e| e.path())
                .collect();
            versions.sort();
            versions
                .into_iter()
                .rev()
                .map(|p| p.join(host).join("rc.exe"))
                .find(|p| p.is_file())
        })
        .expect("Windows SDK resource compiler not found; set RC to rc.exe");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let out = out_dir.join("hbsync.res");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let numbers = format!("{},0", version.replace('.', ","));
    let source = std::fs::read_to_string("assets/hbsync.rc")
        .unwrap()
        .replace("@VERSION_NUM@", &numbers)
        .replace("@VERSION@", &version);
    let resource = out_dir.join("hbsync.rc");
    std::fs::write(&resource, source).unwrap();
    let status = Command::new(rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&out)
        .arg(resource)
        .status()
        .expect("run rc.exe");
    assert!(status.success(), "Windows icon resource compilation failed");
    println!("cargo:rustc-link-arg={}", out.display());
}
