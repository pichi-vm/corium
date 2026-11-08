// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use std::fs;
use std::os::unix::process::CommandExt as _;
use std::path::Path;

use crate::disk::{Disk, ROOT_PARTITION_TYPE};
use crate::filesystem::Filesystem;
use carapace::{Assemble, DevId};
use devmap_core::Span;
use devmap_linux::{Control, Table, targets::Linear};
use rustix::mount::{
    MountFlags, MountPropagationFlags, UnmountFlags, mount, mount_change, mount_move, unmount,
};

use crate::progress::{Phase, Progress};

const SYSROOT: &str = "/sysroot";

/// A filesystem view of a block-device extent, retained by its mount.
#[derive(Debug)]
pub(crate) struct MountView {
    control: Control,
    id: DevId,
}

impl TryFrom<&fs::File> for MountView {
    type Error = std::io::Error;

    fn try_from(source: &fs::File) -> Result<Self, Self::Error> {
        let (start, sectors) = if let Some(disk) = Disk::probe(
            &mut &*source,
            carapace::ZERO_SIZE_BYTES,
            ROOT_PARTITION_TYPE,
        )? {
            if rustix::fs::ioctl_blksszget(source)? != 4096 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "partitioned carapace requires 4096-byte logical blocks",
                ));
            }
            let root = disk.root;
            log::debug!(target: "corium", start = root.start, sectors = root.sectors; "root partition selected");
            (root.start, root.sectors)
        } else {
            (0, carapace::ZERO_SIZE_SECTORS)
        };
        Self::new(source, start, sectors, "corium-root", true)
    }
}

impl MountView {
    pub(crate) fn path(&self) -> String {
        format!("/dev/dm-{}", self.id.minor())
    }

    pub(crate) fn new(
        source: &fs::File,
        start: u64,
        sectors: u64,
        name: &str,
        readonly: bool,
    ) -> std::io::Result<Self> {
        let control = Control::open()?;
        let target = Linear {
            device: source.metadata()?.try_into()?,
            offset: start,
        };
        let table = Table::<128>::default().add(
            Span::new(0, sectors),
            &target,
            control.version("linear")?,
        )?;
        // Corium owns these views as PID 1. A name collision fails rather than
        // taking ownership of an existing mapping.
        let view = Self {
            id: control.create(name)?,
            control,
        };
        view.control.load(view.id, table, readonly)?;
        view.control.suspend(view.id, false)?;
        Ok(view)
    }
}

impl Drop for MountView {
    fn drop(&mut self) {
        // Remove failed/unmounted views immediately; a successful mount keeps
        // the view and its dependencies alive until the filesystem is released.
        if let Err(error) = self.control.remove(self.id, true) {
            log::warn!(target: "corium", error:% = error; "cannot defer mount view removal");
        }
    }
}

/// Establish the verified root, prepare optional persistent instance directories, and execute /sbin/init.
/// Returns on error or requested shutdown. Must run as PID 1 in an initramfs.
/// Only declared state mounts survive handoff; temporary API mounts do not.
///
/// # Errors
/// Returns an operator-facing diagnostic on non-PID-1 invocation or failure
/// during setup, configuration validation, module loading, discovery, assembly,
/// mounting, pivoting, or exec. Errors are fatal; callers do not recover by kind.
pub fn boot() -> Result<(), String> {
    let mut progress = Progress::new();
    if rustix::process::getpid() != rustix::process::Pid::INIT {
        return Err("must run as PID 1".into());
    }

    // Set up the console first so subsequent failures can be reported.
    mount_api("devtmpfs", "/dev")?;
    open_console()?;
    let uname = rustix::system::uname();
    let release = uname
        .release()
        .to_str()
        .map_err(|e| format!("kernel release: {e}"))?;
    require_kernel(release)?;
    mount_api("proc", "/proc")?;
    mount_api("sysfs", "/sys")?;

    let text =
        fs::read_to_string("/proc/bootconfig").map_err(|e| format!("read bootconfig: {e}"))?;
    let config = crate::bootconfig::BootConfig::parse(&text)?;
    log::set_max_level(config.log_level()?);
    config.check_keys()?;
    let root = config.root_hash()?;
    progress.shutdown = config.shutdown()?;
    if progress.complete(Phase::Started) {
        return Ok(());
    }
    crate::modules::load(&Path::new("/usr/lib/modules").join(release))?;
    if progress.complete(Phase::Modules) {
        return Ok(());
    }
    let chain = crate::discovery::discover(&root).map_err(|e| format!("discover root: {e}"))?;
    if progress.complete(Phase::Discovered) {
        return Ok(());
    }
    log::debug!(target: "corium", "assembling root");
    let device = chain
        .assemble()
        .map_err(|e| format!("assemble root: {e}"))?;
    if progress.complete(Phase::Assembled) {
        return Ok(());
    }
    let view = MountView::try_from(&device).map_err(|e| format!("create root mount view: {e}"))?;
    let source = view.path();
    log::debug!(target: "corium", device = source.as_str(); "root device assembled");
    fs::create_dir_all(SYSROOT).map_err(|e| format!("create {SYSROOT}: {e}"))?;
    let filesystems = fs::read_to_string("/proc/filesystems")
        .map_err(|e| format!("read filesystem drivers: {e}"))?;
    let filesystem = fs::File::open(&source)
        .and_then(Filesystem::probe)
        .map_err(|e| format!("read root superblock: {e}"))?;
    let fstype = mount_root(&filesystems, filesystem, |fstype| {
        mount(
            source.as_str(),
            SYSROOT,
            fstype,
            MountFlags::RDONLY | MountFlags::SILENT,
            None,
        )
        .map_err(std::io::Error::from)
    })?;
    // The mount retains its own view, leaving the shared chain free for stacking.
    drop(view);
    drop(device);

    let bindings =
        crate::state::bindings(Path::new(SYSROOT)).map_err(|e| format!("read /var/.bind: {e}"))?;

    if progress.complete(Phase::Mounted) {
        return Ok(());
    }
    pivot()?;
    if progress.complete(Phase::Pivoted) {
        return Ok(());
    }
    if let Some(entries) = bindings {
        crate::state::mount_state(fstype, &entries)
            .map_err(|e| format!("prepare persistent state: {e}"))?;
    }
    for target in ["/proc", "/sys", "/dev"] {
        unmount(target, UnmountFlags::DETACH).map_err(|e| format!("detach {target}: {e}"))?;
    }
    if progress.complete(Phase::State) {
        return Ok(());
    }
    // Failed exec retains the console for the fatal diagnostic.
    let error = std::process::Command::new("/sbin/init")
        .args(std::env::args_os().skip(1))
        .exec();
    Err(format!("exec /sbin/init: {error}"))
}

fn mount_api(fstype: &str, target: &str) -> Result<(), String> {
    fs::create_dir_all(target).map_err(|e| format!("create {target}: {e}"))?;
    mount(
        fstype,
        target,
        fstype,
        MountFlags::NOSUID | MountFlags::NOEXEC,
        None,
    )
    .map_err(|e| format!("mount {target}: {e}"))
}

fn open_console() -> Result<(), String> {
    let console = rustix::fs::open(
        "/dev/console",
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|e| format!("open console: {e}"))?;
    // Ensure dropping the original file cannot close one of our stdio targets
    // when the kernel started /init with descriptors 0, 1, or 2 missing.
    let source = rustix::io::fcntl_dupfd_cloexec(&console, 3)
        .map_err(|e| format!("duplicate console: {e}"))?;
    drop(console);
    rustix::stdio::dup2_stderr(&source).map_err(|e| format!("console stderr: {e}"))?;
    rustix::stdio::dup2_stdout(&source).map_err(|e| format!("console stdout: {e}"))?;
    rustix::stdio::dup2_stdin(&source).map_err(|e| format!("console stdin: {e}"))?;
    for fd in [
        rustix::stdio::stdin(),
        rustix::stdio::stdout(),
        rustix::stdio::stderr(),
    ] {
        rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC)
            .map_err(|e| format!("console close-on-exec: {e}"))?;
    }
    Ok(())
}

fn require_kernel(release: &str) -> Result<(), String> {
    if release
        .split('.')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .is_some_and(|major| major >= 7)
    {
        Ok(())
    } else {
        Err(format!(
            "Linux 7.0+ required for initramfs pivot_root (running {release})"
        ))
    }
}

fn mount_root(
    filesystems: &str,
    filesystem: Option<Filesystem>,
    mut attempt: impl FnMut(&str) -> std::io::Result<()>,
) -> Result<&str, String> {
    if let Some(filesystem) = filesystem {
        let driver = filesystem.driver(filesystems)?;
        attempt(driver)
            .map_err(|error| format!("cannot mount {driver} root read-only: {error}"))?;
        log::debug!(target: "corium", filesystem = driver; "root filesystem mounted");
        return Ok(driver);
    }
    let mut errors = Vec::new();
    for line in filesystems.lines() {
        let mut fields = line.split_whitespace();
        let Some(fstype) = fields.next() else {
            continue;
        };
        if fstype == "nodev" || fields.next().is_some() {
            continue;
        }
        log::trace!(target: "corium", filesystem = fstype; "trying root filesystem");
        match attempt(fstype) {
            Ok(()) => {
                log::debug!(target: "corium", filesystem = fstype; "root filesystem mounted");
                return Ok(fstype);
            }
            Err(error) => {
                log::trace!(target: "corium", filesystem = fstype, error:% = error; "root mount attempt failed");
                errors.push(format!("{fstype}: {error}"));
            }
        }
    }
    if errors.is_empty() {
        Err("no block filesystem drivers available".into())
    } else {
        Err(format!(
            "cannot mount root read-only: {}",
            errors.join("; ")
        ))
    }
}

fn pivot() -> Result<(), String> {
    // pivot_root requires private mounts. Do this before detach so no unmount
    // can propagate outside this tree.
    mount_change(
        "/",
        MountPropagationFlags::PRIVATE | MountPropagationFlags::REC,
    )
    .map_err(|e| format!("make mounts private: {e}"))?;
    for target in ["/proc", "/sys", "/dev"] {
        mount_move(target, format!("{SYSROOT}{target}"))
            .map_err(|e| format!("move {target} into root: {e}"))?;
    }
    std::env::set_current_dir(SYSROOT).map_err(|e| format!("chdir {SYSROOT}: {e}"))?;
    // Linux 7.0+ permits pivoting out of initramfs. Stacking the old root on
    // the new one needs no writable /old directory in the read-only root.
    rustix::process::pivot_root(".", ".").map_err(|e| format!("pivot root: {e}"))?;
    unmount(".", UnmountFlags::DETACH).map_err(|e| format!("detach old root: {e}"))?;
    std::env::set_current_dir("/").map_err(|e| format!("chdir /: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_kernel_with_initramfs_pivot_support() {
        for version in ["7.0.0", "7.2.11-custom", "8.0"] {
            require_kernel(version).unwrap();
        }
        for version in ["6.19.0", "", "unknown"] {
            assert!(require_kernel(version).is_err());
        }
    }

    #[test]
    fn tries_only_block_filesystems_and_stops_at_success() {
        let mut attempts = Vec::new();
        mount_root(
            "nodev\tproc\n\txfs\nnodev\ttmpfs\n\text4\n\tbtrfs\n",
            None,
            |name| {
                attempts.push(name.to_string());
                if name == "ext4" {
                    Ok(())
                } else {
                    Err(std::io::Error::other("wrong filesystem"))
                }
            },
        )
        .unwrap();
        assert_eq!(attempts, ["xfs", "ext4"]);
    }

    #[test]
    fn all_mount_failures_are_reported() {
        let error = mount_root("xfs\next4\n", None, |_| {
            Err(std::io::Error::other("bad root"))
        })
        .unwrap_err();
        assert!(error.contains("xfs: bad root"));
        assert!(error.contains("ext4: bad root"));
        for input in ["nodev proc\n", "", "  \n\t"] {
            assert_eq!(
                mount_root(input, None, |_| panic!("must not mount nodev")).unwrap_err(),
                "no block filesystem drivers available"
            );
        }
    }

    #[test]
    fn ext_superblock_uses_ext4_without_incompatible_probes() {
        let mut attempts = Vec::new();
        assert_eq!(
            mount_root("ext3\next2\next4\n", Some(Filesystem::Ext), |name| {
                attempts.push(name.to_owned());
                Ok(())
            })
            .unwrap(),
            "ext4"
        );
        assert_eq!(attempts, ["ext4"]);
        let error = mount_root("ext3\next2\next4\n", Some(Filesystem::Ext), |name| {
            assert_eq!(name, "ext4");
            Err(std::io::Error::other("corrupt superblock"))
        })
        .unwrap_err();
        assert!(error.contains("corrupt superblock"));
    }
}
