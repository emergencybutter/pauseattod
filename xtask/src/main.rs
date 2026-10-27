//! Build helper: `cargo xtask dist [--target <triple>]... [--install <X-Plane folder>]`.
//!
//! Builds the plugin in release mode and lays it out the way X-Plane expects:
//! `dist/PauseAtToD/64/{win,lin,mac}.xpl`. Passing two macOS targets produces a
//! universal `mac.xpl` with `lipo`. With `--install`, the folder is also
//! copied to `<X-Plane>/Resources/plugins/PauseAtToD`.

use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::{env, fs};

const PLUGIN_DIR: &str = "PauseAtToD";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("dist") => dist(&args[1..]),
        _ => Err("usage: cargo xtask dist [--target <triple>]... [--install <X-Plane folder>]".into()),
    };
    if let Err(e) = result {
        eprintln!("xtask: {e}");
        exit(1);
    }
}

fn dist(args: &[String]) -> Result<(), String> {
    let mut targets: Vec<String> = Vec::new();
    let mut install: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => targets.push(it.next().ok_or("--target needs a value")?.clone()),
            "--install" => install = Some(it.next().ok_or("--install needs a folder")?.into()),
            other => return Err(format!("unknown argument {other}")),
        }
    }

    let os = match targets.first() {
        Some(t) => os_of(t),
        None => env::consts::OS,
    };
    if targets.iter().any(|t| os_of(t) != os) {
        return Err("all --target values must be for the same OS".into());
    }
    if targets.len() > 1 && os != "macos" {
        return Err("several --target values only make sense for a universal macOS build".into());
    }
    let (lib, xpl) = match os {
        "windows" => ("pauseattod.dll", "win.xpl"),
        "macos" => ("libpauseattod.dylib", "mac.xpl"),
        "linux" => ("libpauseattod.so", "lin.xpl"),
        other => return Err(format!("unsupported OS {other}")),
    };

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut cargo = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo.current_dir(&root).args(["build", "--release", "-p", "pauseattod"]);
    for t in &targets {
        cargo.args(["--target", t]);
    }
    if !cargo.status().map_err(|e| e.to_string())?.success() {
        return Err("cargo build failed".into());
    }

    let built: Vec<PathBuf> = if targets.is_empty() {
        vec![root.join("target").join("release").join(lib)]
    } else {
        targets.iter().map(|t| root.join("target").join(t).join("release").join(lib)).collect()
    };

    let out_dir = root.join("dist").join(PLUGIN_DIR).join("64");
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let out = out_dir.join(xpl);
    if built.len() > 1 {
        // Universal binary, e.g. x86_64 + arm64 for Intel and Apple Silicon Macs.
        let status = Command::new("lipo")
            .arg("-create")
            .args(&built)
            .arg("-output")
            .arg(&out)
            .status()
            .map_err(|e| format!("lipo: {e}"))?;
        if !status.success() {
            return Err("lipo failed".into());
        }
    } else {
        fs::copy(&built[0], &out).map_err(|e| format!("copy {}: {e}", built[0].display()))?;
    }
    println!("built {}", out.display());

    if let Some(xp) = install {
        let plugins = xp.join("Resources").join("plugins");
        if !plugins.is_dir() {
            return Err(format!("{} is not an X-Plane folder (no Resources/plugins)", xp.display()));
        }
        let dest = plugins.join(PLUGIN_DIR).join("64");
        fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        let target = dest.join(xpl);
        if fs::copy(&out, &target).is_err() {
            // A running X-Plane locks the loaded plugin, but Windows still lets
            // us rename it. X-Plane doesn't load files ending in `.old`.
            let old = (0..100)
                .map(|i| dest.join(format!("{xpl}.{i}.old")))
                .find(|p| !p.exists() || fs::remove_file(p).is_ok())
                .ok_or("install: too many old plugin copies")?;
            fs::rename(&target, &old).map_err(|e| format!("install: cannot move the old plugin aside: {e}"))?;
            fs::copy(&out, &target).map_err(|e| format!("install: {e}"))?;
        }
        println!("installed to {} (restart X-Plane to load it)", target.display());
    }
    Ok(())
}

fn os_of(triple: &str) -> &'static str {
    if triple.contains("windows") {
        "windows"
    } else if triple.contains("apple") {
        "macos"
    } else {
        "linux"
    }
}
