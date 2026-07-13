use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must live directly under the workspace root")
        .to_path_buf()
}

fn build_kernel(root: &Path, release: bool) -> PathBuf {
    let kernel_dir = root.join("kernel");
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&kernel_dir).arg("build");
    if release {
        cmd.arg("--release");
    }
    let status = cmd.status().expect("failed to spawn cargo build for kernel");
    if !status.success() {
        panic!("kernel build failed with status: {status}");
    }

    let profile_dir = if release { "release" } else { "debug" };
    let bin_path = kernel_dir
        .join("target")
        .join("x86_64-unknown-none")
        .join(profile_dir)
        .join("kernel");
    if !bin_path.exists() {
        panic!("expected kernel binary at {bin_path:?} but it does not exist");
    }
    bin_path
}

fn build_disk_image(kernel_bin: &Path) -> PathBuf {
    let image_path = kernel_bin.with_extension("img");
    bootloader::BiosBoot::new(kernel_bin)
        .create_disk_image(&image_path)
        .expect("failed to create bootable BIOS disk image");
    image_path
}

fn run_qemu(image: &Path) {
    let status = Command::new("qemu-system-x86_64")
        .arg("-drive")
        .arg(format!("format=raw,file={}", image.display()))
        .arg("-serial")
        .arg("stdio")
        .arg("-no-reboot")
        .arg("-no-shutdown")
        .status()
        .expect("failed to spawn qemu-system-x86_64 (is it on PATH?)");
    if !status.success() {
        eprintln!("qemu exited with status: {status}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let release = args.iter().any(|a| a == "--release");
    let root = workspace_root();

    match args.first().map(String::as_str) {
        Some("run") | None => {
            let kernel_bin = build_kernel(&root, release);
            let image = build_disk_image(&kernel_bin);
            println!("Booting {} in QEMU...", image.display());
            run_qemu(&image);
        }
        Some("build") => {
            let kernel_bin = build_kernel(&root, release);
            let image = build_disk_image(&kernel_bin);
            println!("Built disk image: {}", image.display());
        }
        Some(other) => {
            eprintln!("unknown xtask subcommand: {other}");
            eprintln!("usage: cargo xtask [run|build] [--release]");
            std::process::exit(1);
        }
    }
}
