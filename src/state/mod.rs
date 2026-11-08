// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Persistent instance directories with image-owned read-only /var bindings.

mod disk;

use crate::boot::MountView;
use gpt::disk::LogicalBlockSize;
use rustix::mount::{MountFlags, UnmountFlags, mount, mount_bind, mount_bind_recursive, unmount};
use std::io::{Seek, SeekFrom};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, chown};
use std::{
    fs, io,
    path::Path,
    process::{Command, Stdio},
};

const DIRECTORIES: [&str; 4] = ["var", "home", "root", "srv"];

fn device() -> io::Result<Option<fs::File>> {
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
    if candidates.is_empty() {
        return Ok(None);
    }
    if candidates.len() != 1 {
        return Err(io::Error::other(format!(
            "persistent state needs exactly one writable whole virtio disk; found {}",
            candidates.len()
        )));
    }
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&candidates[0])
        .map(Some)
}

/// Read policy only from the verified image, before hiding its /var.
pub(crate) fn bindings(root: &Path) -> io::Result<Vec<String>> {
    let manifest = root.join("var/.bind");
    let metadata = match fs::symlink_metadata(&manifest) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Err(io::Error::other("/var/.bind must be a regular file"));
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
    Ok(entries)
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

/// Initialize only missing state directories; retain existing instance metadata.
fn state_directory(image: &Path, state: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(image)?;
    match fs::DirBuilder::new().mode(metadata.mode()).create(state) {
        Ok(()) => {
            chown(state, Some(metadata.uid()), Some(metadata.gid()))?;
            fs::set_permissions(state, metadata.permissions())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => directory_type(state),
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

pub(crate) fn mount_state(fstype: &str, entries: &[String]) -> io::Result<()> {
    let Some(mut device) = device()? else {
        return Ok(());
    };
    for directory in DIRECTORIES.into_iter().chain(["tmp", "dev", "proc", "sys"]) {
        directory_type(&Path::new("/").join(directory))?;
    }
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
    for name in DIRECTORIES {
        state_directory(&Path::new("/").join(name), &Path::new("/tmp").join(name))?;
    }
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
    for name in DIRECTORIES {
        mount_bind_recursive(Path::new("/tmp").join(name), Path::new("/").join(name))?;
    }
    // Detach staging submounts as well; the final mounts retain their own references.
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
    fn optional_manifest_requires_real_sources() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        assert_eq!(bindings(root).unwrap(), Vec::<String>::new());
        for path in DIRECTORIES.into_iter().chain(["tmp", "dev", "proc", "sys"]) {
            fs::create_dir(root.join(path)).unwrap();
        }
        fs::write(root.join("var/.bind"), "").unwrap();
        assert_eq!(bindings(root).unwrap(), Vec::<String>::new());
        fs::write(root.join("var/.bind"), "missing").unwrap();
        assert!(bindings(root).is_err());
        fs::write(root.join("var/value"), "metadata").unwrap();
        symlink("value", root.join("var/link")).unwrap();
        fs::write(root.join("var/.bind"), "link").unwrap();
        assert!(bindings(root).is_err());
        fs::write(root.join("var/.bind"), "value").unwrap();
        assert_eq!(bindings(root).unwrap(), vec!["value".to_string()]);
        fs::remove_file(root.join("var/.bind")).unwrap();
        symlink("value", root.join("var/.bind")).unwrap();
        assert!(bindings(root).is_err());
    }

    #[test]
    fn state_directories_inherit_image_metadata_only_when_new() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("image");
        let state = temp.path().join("state");
        fs::create_dir(&image).unwrap();
        fs::set_permissions(&image, fs::Permissions::from_mode(0o700)).unwrap();
        state_directory(&image, &state).unwrap();
        let metadata = fs::metadata(&state).unwrap();
        assert_eq!(metadata.mode() & 0o7777, 0o700);
        assert_eq!(metadata.uid(), fs::metadata(&image).unwrap().uid());
        assert_eq!(metadata.gid(), fs::metadata(&image).unwrap().gid());
        fs::write(state.join("retained"), "keep").unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o750)).unwrap();
        state_directory(&image, &state).unwrap();
        assert_eq!(fs::metadata(&state).unwrap().mode() & 0o7777, 0o750);
        assert_eq!(fs::read_to_string(state.join("retained")).unwrap(), "keep");
        fs::remove_dir_all(&state).unwrap();
        symlink(&image, &state).unwrap();
        assert!(state_directory(&image, &state).is_err());
        fs::remove_file(&state).unwrap();
        fs::write(&state, "keep").unwrap();
        assert!(state_directory(&image, &state).is_err());
        assert_eq!(fs::read_to_string(&state).unwrap(), "keep");
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
