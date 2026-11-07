// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Persistent /var on a Corium state filesystem.

mod disk;

use crate::boot::MountView;
use gpt::disk::LogicalBlockSize;
use rustix::mount::{MountFlags, UnmountFlags, mount, mount_bind, unmount};
use std::io::{Seek, SeekFrom};
use std::{
    fs, io,
    path::Path,
    process::{Command, Stdio},
};

fn device() -> io::Result<fs::File> {
    let mut candidates = Vec::new();
    for entry in fs::read_dir("/sys/class/block")? {
        let entry = entry?;
        let path = entry.path();
        // Match the driver, not a disk letter or enumeration order.
        if path.join("partition").exists() || fs::read_to_string(path.join("ro"))?.trim() != "0" {
            continue;
        }
        let Ok(driver) = fs::canonicalize(path.join("device/driver")) else {
            continue;
        };
        if driver.file_name().is_some_and(|name| name == "virtio_blk") {
            candidates.push(Path::new("/dev").join(entry.file_name()));
        }
    }
    if candidates.len() != 1 {
        return Err(io::Error::other(format!(
            "persistent /var needs exactly one writable whole virtio disk; found {}",
            candidates.len()
        )));
    }
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&candidates[0])
}

/// An empty image manifest opts into persistent /var.
pub(crate) fn enabled(root: &Path) -> io::Result<bool> {
    let manifest = root.join("var/.bind");
    let metadata = match fs::symlink_metadata(&manifest) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Err(io::Error::other("/var/.bind must be a regular file"));
    }
    for directory in ["var", "tmp", "dev", "proc", "sys"] {
        directory_type(&root.join(directory))?;
    }
    if fs::read_to_string(manifest)?
        .lines()
        .any(|line| !line.is_empty())
    {
        return Err(io::Error::other("nonempty /var/.bind is not supported"));
    }
    Ok(true)
}

fn directory_type(path: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(path)
        .map_err(|error| io::Error::other(format!("{}: {error}", path.display())))?
        .is_dir()
    {
        return Err(io::Error::other(format!(
            "{} must be a real directory",
            path.display()
        )));
    }
    Ok(())
}

fn directory(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => directory_type(path),
        Err(error) => Err(error),
    }
}

/// Execute the verified image's formatter after pivot, keeping Corium as PID 1.
fn format(fstype: &str, path: &str) -> io::Result<()> {
    let console = fs::OpenOptions::new().write(true).open("/dev/console")?;
    let stderr = console.try_clone()?;
    let status = Command::new(format!("/usr/sbin/mkfs.{fstype}"))
        .arg(path)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::null())
        .stdout(console)
        .stderr(stderr)
        .status()
        .map_err(|error| {
            io::Error::other(format!(
                "execute mkfs.{fstype}: {error}; state disk will not be reformatted"
            ))
        })?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "mkfs.{fstype} failed: {status}; state disk will not be reformatted"
        )));
    }
    Ok(())
}

pub(crate) fn mount_var(fstype: &str) -> io::Result<()> {
    let mut device = device()?;
    let size = device.seek(SeekFrom::End(0))?;
    let sector = match rustix::fs::ioctl_blksszget(&device)? {
        512 => LogicalBlockSize::Lb512,
        4096 => LogicalBlockSize::Lb4096,
        value => {
            return Err(io::Error::other(format!(
                "unsupported state sector size: {value}"
            )));
        }
    };
    let (partition, created) = disk::prepare(&mut device, size, sector)?;
    let factor = u64::from(sector) / 512;
    let view = MountView::new(
        &device,
        partition.first_lba * factor,
        (partition.last_lba - partition.first_lba + 1) * factor,
        "corium-state",
        false,
    )?;
    let source = view.path();
    if created {
        format(fstype, &source)?;
    }

    mount(
        source.as_str(),
        "/tmp",
        fstype,
        MountFlags::NOSUID | MountFlags::NODEV,
        None,
    )
    .map_err(|error| {
        io::Error::other(format!(
            "mount state as {fstype}: {error}; refusing to reformat"
        ))
    })?;
    drop(view);
    directory(Path::new("/tmp/var"))?;
    mount_bind("/tmp/var", "/var")?;
    // The /var bind retains the filesystem after staging is detached.
    unmount("/tmp", UnmountFlags::DETACH)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn requires_an_empty_regular_image_manifest() {
        let root = tempfile::tempdir().unwrap();
        assert!(!enabled(root.path()).unwrap());
        for name in ["var", "tmp", "dev", "proc", "sys"] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        let manifest = root.path().join("var/.bind");
        fs::write(&manifest, "\n\r\n").unwrap();
        assert!(enabled(root.path()).unwrap());
        fs::write(&manifest, "lib/dpkg\n").unwrap();
        assert!(enabled(root.path()).is_err());
        fs::write(&manifest, "").unwrap();
        fs::remove_dir(root.path().join("tmp")).unwrap();
        symlink("var", root.path().join("tmp")).unwrap();
        assert!(enabled(root.path()).is_err());
        fs::remove_file(root.path().join("tmp")).unwrap();
        fs::create_dir(root.path().join("tmp")).unwrap();
        fs::remove_file(&manifest).unwrap();
        symlink("/dev/null", &manifest).unwrap();
        assert!(enabled(root.path()).is_err());
    }

    #[test]
    fn rejects_symlink_state_directory_without_overwriting() {
        let root = tempfile::tempdir().unwrap();
        let var = root.path().join("var");
        symlink("/", &var).unwrap();
        assert!(directory(&var).is_err());
        assert_eq!(fs::read_link(&var).unwrap(), Path::new("/"));
        fs::remove_file(&var).unwrap();
        directory(&var).unwrap();
        fs::write(var.join("retained"), "keep").unwrap();
        directory(&var).unwrap();
        assert_eq!(fs::read_to_string(var.join("retained")).unwrap(), "keep");
    }
}
