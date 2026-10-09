//! Shared source fingerprint: dev rebuilds must detect an older resident daemon
//! even when HEAD has not moved. Both build scripts hash the same daemon inputs.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn emit(root: &Path) {
    let mut files = vec![
        root.join("Cargo.toml"),
        root.join("Cargo.lock"),
        root.join("build-support/daemon_version.rs"),
    ];
    collect(&root.join("crates"), &mut files);
    files.sort();
    let mut hash = 0xcbf29ce484222325_u64;
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        let relative = path.strip_prefix(root).expect("workspace input");
        // Normalize separators so the same checkout has the same id on Windows.
        let name = relative.to_string_lossy().replace('\\', "/");
        let bytes = std::fs::read(&path).expect("read daemon build input");
        for byte in name.bytes().chain([0]).chain(bytes).chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    let sha = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "nogit".to_string());
    println!("cargo:rustc-env=IYAGI_GIT_SHA={sha}-{hash:016x}");
    // Resolve worktree refs as well as ordinary repositories.
    for name in ["HEAD", "index"] {
        if let Ok(out) = Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "--git-path", name])
            .output()
        {
            if out.status.success() {
                let path = String::from_utf8_lossy(&out.stdout);
                println!(
                    "cargo:rerun-if-changed={}",
                    root.join(path.trim()).display()
                );
            }
        }
    }
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    // Watching directories also detects newly added/deleted source files.
    println!("cargo:rerun-if-changed={}", dir.display());
    for entry in std::fs::read_dir(dir).expect("read daemon source directory") {
        let entry = entry.expect("read daemon source entry");
        let kind = entry.file_type().expect("daemon source file type");
        let path = entry.path();
        if kind.is_dir() {
            let name = entry.file_name();
            if name != "target" && name != ".git" && name != "node_modules" {
                collect(&path, files);
            }
        } else if kind.is_file() {
            files.push(path);
        }
    }
}
