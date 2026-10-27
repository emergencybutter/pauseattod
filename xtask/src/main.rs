//! Build helper: `cargo xtask dist [--install <X-Plane folder>]`.
//!
//! Builds the plugin in release mode and lays it out the way X-Plane expects:
//! `dist/PauseAtToD/64/{win,lin,mac}.xpl`. With `--install`, the folder is also
//! copied to `<X-Plane>/Resources/plugins/PauseAtToD`.

use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::{env, fs};

const PLUGIN_DIR: &str = "PauseAtToD";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("dist") => dist(&args[1..]),
        _ => Err("usage: cargo xtask dist [--target <triple>] [--install <X-Plane folder>]".into()),
    };
    if let Err(e) = result {
        eprintln!("xtask: {e}");
        exit(1);
    }
}

fn dist(args: &[String]) -> Result<(), String> {
    let mut target: Option<String> = None;
    let mut install: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => target = Some(it.next().ok_or("--target needs a value")?.clone()),
            "--install" => install = Some(it.next().ok_or("--install needs a folder")?.into()),
            other => return Err(format!("unknown argument {other}")),
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut cargo = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo.current_dir(&root).args(["build", "--release", "-p", "pauseattod"]);
    if let Some(t) = &target {
        cargo.args(["--target", t]);
    }
    if !cargo.status().map_err(|e| e.to_string())?.success() {
        return Err("cargo build failed".into());
    }

    let os = target.as_deref().map(os_of).unwrap_or(env::consts::OS);
    let (lib, xpl) = match os {
        "windows" => ("pauseattod.dll", "win.xpl"),
        "macos" => ("libpauseattod.dylib", "mac.xpl"),
        "linux" => ("libpauseattod.so", "lin.xpl"),
        other => return Err(format!("unsupported OS {other}")),
    };
    let mut built = root.join("target");
    if let Some(t) = &target {
        built.push(t);
    }
    built.push("release");
    built.push(lib);

    let out_dir = root.join("dist").join(PLUGIN_DIR).join("64");
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let out = out_dir.join(xpl);
    fs::copy(&built, &out).map_err(|e| format!("copy {}: {e}", built.display()))?;
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
