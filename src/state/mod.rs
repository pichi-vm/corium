// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Persistent /var with image-owned read-only bind mounts.

mod disk;

use crate::boot::MountView;
use gpt::disk::LogicalBlockSize;
use rustix::mount::{MountFlags, UnmountFlags, mount, mount_bind, mount_bind_recursive, unmount};
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

/// Read policy only from the verified image, before hiding its /var.
pub(crate) fn bindings(root: &Path) -> io::Result<Option<Vec<String>>> {
    let manifest = root.join("var/.bind");
    let metadata = match fs::symlink_metadata(&manifest) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Err(io::Error::other("/var/.bind must be a regular file"));
    }
    for directory in ["var", "tmp", "dev", "proc", "sys"] {
        directory_type(&root.join(directory))?;
    }
    let entries = parse(&fs::read_to_string(manifest)?)?;
    for entry in &entries {
        let mut path = root.join("var");
        let parts: Vec<_> = entry.split('/').collect();
        for (index, part) in parts.iter().enumerate() {
            path.push(part);
            if index + 1 < parts.len() {
                directory_type(&path)?;
            } else {
                let metadata = fs::symlink_metadata(&path)
                    .map_err(|error| io::Error::other(format!("{}: {error}", path.display())))?;
                if !metadata.is_file() && !metadata.is_dir() {
                    return Err(io::Error::other(format!(
                        "invalid bind source: {}",
                        path.display()
                    )));
                }
            }
        }
    }
    Ok(Some(entries))
}

fn parse(text: &str) -> io::Result<Vec<String>> {
    let mut entries: Vec<String> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        if line.trim() != line
            || line.contains('\0')
            || line.split('/').any(|part| matches!(part, "" | "." | ".."))
            || line.split('/').next() == Some(".bind")
        {
            return Err(io::Error::other(format!(
                "invalid /var/.bind line {}: {line:?}",
                number + 1
            )));
        }
        if entries
            .iter()
            .any(|other| Path::new(line).starts_with(other) || Path::new(other).starts_with(line))
        {
            return Err(io::Error::other(format!(
                "overlapping /var/.bind line {}: {line}",
                number + 1
            )));
        }
        entries.push(line.to_owned());
    }
    Ok(entries)
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

/// Create mount targets without following symlinks or replacing existing data.
fn target(root: &Path, entry: &str, is_directory: bool) -> io::Result<()> {
    let mut path = root.to_owned();
    directory_type(&path)?;
    let parts: Vec<_> = entry.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        path.push(part);
        if index + 1 < parts.len() || is_directory {
            directory(&path)?;
        } else {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if !fs::symlink_metadata(&path)?.is_file() {
                        return Err(io::Error::other(format!(
                            "{} must be a regular file",
                            path.display()
                        )));
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
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

pub(crate) fn mount_var(fstype: &str, entries: &[String]) -> io::Result<()> {
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
    for entry in entries {
        let source = Path::new("/var").join(entry);
        target(
            Path::new("/tmp/var"),
            entry,
            fs::symlink_metadata(&source)?.is_dir(),
        )
        .map_err(|error| io::Error::other(format!("prepare bind target /var/{entry}: {error}")))?;
        // The source mount and backing verified filesystem are read-only.
        mount_bind(&source, Path::new("/tmp/var").join(entry))
            .map_err(|error| io::Error::other(format!("bind /var/{entry}: {error}")))?;
    }
    mount_bind_recursive("/tmp/var", "/var")?;
    // Detach staging submounts as well; the final /var retains its own references.
    unmount("/tmp", UnmountFlags::DETACH)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn manifest_rejects_escaping_and_overlapping_paths() {
        for text in [
            "/lib",
            "../etc",
            "lib/../etc",
            "lib//dpkg",
            "lib/",
            ".",
            ".bind",
            ".bind/x",
            " lib",
            "lib ",
            "lib\0x",
            "lib\nlib",
            "lib\nlib/dpkg",
            "lib/dpkg\nlib",
        ] {
            assert!(parse(text).is_err(), "{text:?}");
        }
        assert_eq!(
            parse("lib/dpkg\r\n\nlib/shells.state\n").unwrap(),
            ["lib/dpkg", "lib/shells.state"]
        );
        assert_eq!(parse("").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn targets_preserve_files_and_reject_symlinks_and_type_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        target(root, "lib/data", false).unwrap();
        fs::write(root.join("lib/data"), "retained").unwrap();
        target(root, "lib/data", false).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("lib/data")).unwrap(),
            "retained"
        );
        assert!(target(root, "lib/data", true).is_err());
        assert!(target(root, "lib", false).is_err());
        symlink("lib", root.join("link")).unwrap();
        assert!(target(root, "link/new", false).is_err());
        assert!(!root.join("lib/new").exists());
        symlink("lib/data", root.join("file-link")).unwrap();
        assert!(target(root, "file-link", false).is_err());
    }

    #[test]
    fn manifest_requires_real_sources_and_opt_in() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        assert!(bindings(root).unwrap().is_none());
        for path in ["var", "tmp", "dev", "proc", "sys"] {
            fs::create_dir(root.join(path)).unwrap();
        }
        fs::write(root.join("var/.bind"), "").unwrap();
        assert_eq!(bindings(root).unwrap(), Some(vec![]));
        fs::write(root.join("var/.bind"), "missing").unwrap();
        assert!(bindings(root).is_err());
        fs::write(root.join("var/value"), "metadata").unwrap();
        symlink("value", root.join("var/link")).unwrap();
        fs::write(root.join("var/.bind"), "link").unwrap();
        assert!(bindings(root).is_err());
        fs::write(root.join("var/.bind"), "value").unwrap();
        assert_eq!(bindings(root).unwrap(), Some(vec!["value".into()]));
    }
}
