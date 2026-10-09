#[path = "../build-support/daemon_version.rs"]
mod daemon_version;

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    daemon_version::emit(&root);

    tauri_build::build()
}
