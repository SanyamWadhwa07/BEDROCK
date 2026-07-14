use std::path::{Path, PathBuf};
use std::process::Command;

/// Must match `simplefs::DEFAULT_TOTAL_BLOCKS` (Phase 7's on-disk
/// filesystem has no `IDENTIFY DEVICE` probe, so the format-time disk size
/// is a compile-time constant shared between the image this builds and the
/// kernel that formats/mounts it).
const FS_IMAGE_BLOCKS: u64 = 1024;
const FS_BLOCK_SIZE: u64 = 4096;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must live directly under the workspace root")
        .to_path_buf()
}

/// Creates the SimpleFS disk image once and reuses it on every subsequent
/// run -- a real file on the host filesystem, so data the kernel writes to
/// it survives across QEMU reboots exactly like a real second hard disk
/// would (the whole point of Phase 7's persistence demo). Delete
/// `fs.img` by hand to force a fresh reformat.
fn ensure_fs_image(root: &Path) -> PathBuf {
    let path = root.join("fs.img");
    if !path.exists() {
        let file = std::fs::File::create(&path).expect("failed to create fs.img");
        file.set_len(FS_IMAGE_BLOCKS * FS_BLOCK_SIZE)
            .expect("failed to size fs.img");
        println!("Created fresh SimpleFS disk image: {}", path.display());
    }
    path
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

/// `boot_image` is the primary master (the bootable kernel image);
/// `fs_image` is attached as the primary slave, entirely separate so the
/// filesystem never shares a disk with the bootloader/kernel it's booting.
fn run_qemu(boot_image: &Path, fs_image: &Path) {
    let status = Command::new("qemu-system-x86_64")
        .arg("-drive")
        .arg(format!("format=raw,file={}", boot_image.display()))
        .arg("-drive")
        .arg(format!("format=raw,file={},index=1", fs_image.display()))
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
            let fs_image = ensure_fs_image(&root);
            println!("Booting {} in QEMU...", image.display());
            run_qemu(&image, &fs_image);
        }
        Some("build") => {
            let kernel_bin = build_kernel(&root, release);
            let image = build_disk_image(&kernel_bin);
            ensure_fs_image(&root);
            println!("Built disk image: {}", image.display());
        }
        Some(other) => {
            eprintln!("unknown xtask subcommand: {other}");
            eprintln!("usage: cargo xtask [run|build] [--release]");
            std::process::exit(1);
        }
    }
}
