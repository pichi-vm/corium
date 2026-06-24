// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! corium `/init` — the initramfs phase of the pichi guest boot (BUILD.md
//! §8.1, MVP scope): mount `/proc`,`/sys`,`/dev`; load the bundled kernel
//! modules; read `root.carapace=<root>` from the kernel cmdline; assemble the
//! root carapace from that trusted root; mount it; and pivot into it (or, if
//! the root carries no `/sbin/init`, print a proof marker and power off — what
//! the MVP boot test asserts).
//!
//! Linux-only; every syscall is a rustix safe wrapper (no `unsafe`, no libc),
//! and carapace assembly comes from the no-unsafe `carapace` crate.

use std::fs;
use std::path::Path;

use corium::{CORIUM_PIVOT, CORIUM_ROOT_OK};
use rustix::mount::{MountFlags, mount};

/// Where the assembled root carapace is mounted before the pivot.
const SYSROOT: &str = "/sysroot";
/// Filesystem type of the root carapace (MVP: bare ext4, built into the
/// kernel; the inner-GPT / systemd-gpt-auto path is later work).
const ROOTFS_TYPE: &str = "ext4";

fn main() {
    setup_mounts();
    load_modules();
    match boot() {
        Ok(()) => {}
        Err(e) => eprintln!("corium: {e}"),
    }
    rustix::fs::sync();
    poweroff();
}

/// Assemble + mount the root carapace, then pivot (or prove + power off).
fn boot() -> Result<(), String> {
    let cmdline = fs::read_to_string("/proc/cmdline").map_err(|e| format!("read cmdline: {e}"))?;
    let root = corium::parse_root_carapace(&cmdline)
        .ok_or("no root.carapace=<root> on the kernel cmdline")?;
    eprintln!("corium: root carapace {root}");

    let dev = carapace::attach("root", root)
        .map_err(|e| format!("assembling root carapace ({root}): {e}"))?;

    let _ = fs::create_dir(SYSROOT);
    mount(
        dev.to_str().unwrap_or_default(),
        SYSROOT,
        ROOTFS_TYPE,
        MountFlags::RDONLY,
        None,
    )
    .map_err(|e| format!("mount root carapace {} -> {SYSROOT}: {e}", dev.display()))?;

    // Proof the carapace mounted: list its root and echo it on the console.
    let entries = list_dir(SYSROOT);
    println!("{CORIUM_ROOT_OK}: {entries}");

    pivot_or_poweroff();
    Ok(())
}

/// `switch_root` into the mounted carapace if it carries `/sbin/init`;
/// otherwise (the MVP boot test's bare rootfs) announce and fall through to
/// power-off. The pivot moves the pseudo-filesystems into the new root, makes
/// it `/`, and execs init.
fn pivot_or_poweroff() {
    use std::os::unix::process::CommandExt as _;
    let init = Path::new(SYSROOT).join("sbin/init");
    if !init.is_file() {
        println!("{CORIUM_PIVOT}: skipped (no /sbin/init in root carapace)");
        return;
    }
    if let Err(e) = switch_root() {
        eprintln!("corium: switch_root failed: {e}");
        return;
    }
    println!("{CORIUM_PIVOT}: exec /sbin/init");
    // `exec` only returns on failure (it replaces this process on success).
    let err = std::process::Command::new("/sbin/init").exec();
    eprintln!("corium: exec /sbin/init failed: {err}");
}

/// Move the pseudo-filesystems into [`SYSROOT`], make it the new root, and
/// chroot into it (the busybox `switch_root` sequence, minus the old-root
/// deletion which the kernel reclaims with the initramfs).
fn switch_root() -> Result<(), String> {
    use rustix::mount::mount_move;
    for fsname in ["proc", "sys", "dev", "run"] {
        let src = format!("/{fsname}");
        let dst = format!("{SYSROOT}/{fsname}");
        if Path::new(&src).is_dir() && Path::new(&dst).is_dir() {
            let _ = mount_move(&src, &dst);
        }
    }
    std::env::set_current_dir(SYSROOT).map_err(|e| format!("chdir {SYSROOT}: {e}"))?;
    mount_move(".", "/").map_err(|e| format!("move . /: {e}"))?;
    rustix::process::chroot(".").map_err(|e| format!("chroot: {e}"))?;
    std::env::set_current_dir("/").map_err(|e| format!("chdir / after chroot: {e}"))?;
    Ok(())
}

/// List a directory's entries (sorted), as a compact console string.
fn list_dir(path: &str) -> String {
    let Ok(rd) = fs::read_dir(path) else {
        return "<unreadable>".to_string();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names.join(",")
}

/// Load the kernel modules bundled in `/modules` in sorted filename order
/// (numeric-prefixed = dependency order). Mirrors conglobate's loader; the
/// root carapace's dm-verity read stack needs dm-verity here.
fn load_modules() {
    let Ok(entries) = fs::read_dir("/modules") else {
        return;
    };
    let mut kos: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "ko"))
        .collect();
    kos.sort();
    for ko in kos {
        if let Ok(f) = fs::File::open(&ko)
            && let Err(e) = rustix::system::finit_module(&f, c"", 0)
        {
            eprintln!("corium: load module {} failed: {e}", ko.display());
        }
    }
}

/// Create the standard mountpoints and mount the pseudo-filesystems.
fn setup_mounts() {
    for dir in ["/proc", "/sys", "/dev", "/run"] {
        let _ = fs::create_dir(dir);
    }
    do_mount("proc", "/proc", "proc");
    do_mount("sysfs", "/sys", "sysfs");
    do_mount("devtmpfs", "/dev", "devtmpfs");
    do_mount("tmpfs", "/run", "tmpfs");
}

fn do_mount(src: &str, target: &str, fstype: &str) {
    if let Err(e) = mount(src, target, fstype, MountFlags::empty(), None) {
        eprintln!("corium: mount {src} -> {target} ({fstype}) failed: {e}");
    }
}

/// Power off the VM. PID 1 must never return.
fn poweroff() -> ! {
    let _ = rustix::system::reboot(rustix::system::RebootCommand::PowerOff);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
