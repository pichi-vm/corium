// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Finish systemd's shutdown root handoff without waiting for deferred DM removal.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use rustix::mount::{UnmountFlags, unmount};
use rustix::system::RebootCommand;

pub(super) fn action(verb: Option<&OsStr>) -> Result<RebootCommand, String> {
    match verb.and_then(OsStr::to_str) {
        Some("poweroff") => Ok(RebootCommand::PowerOff),
        Some("reboot") => Ok(RebootCommand::Restart),
        Some("halt") => Ok(RebootCommand::Halt),
        Some("kexec") => Ok(RebootCommand::Kexec),
        _ => Err(format!("unsupported action: {verb:?}")),
    }
}

fn old_root_mounts(mountinfo: &[u8]) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    for line in mountinfo
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let encoded = line
            .split(|&b| b == b' ')
            .nth(4)
            .ok_or("invalid mountinfo line")?;
        if encoded != b"/oldroot" && !encoded.starts_with(b"/oldroot/") {
            continue;
        }
        let mut decoded = Vec::new();
        let mut bytes = encoded.iter().copied();
        while let Some(byte) = bytes.next() {
            decoded.push(if byte == b'\\' {
                match (bytes.next(), bytes.next(), bytes.next()) {
                    (Some(b'0'), Some(b'4'), Some(b'0')) => b' ',
                    (Some(b'0'), Some(b'1'), Some(b'1')) => b'\t',
                    (Some(b'0'), Some(b'1'), Some(b'2')) => b'\n',
                    (Some(b'1'), Some(b'3'), Some(b'4')) => b'\\',
                    _ => return Err("invalid mountinfo escape".into()),
                }
            } else {
                byte
            });
        }
        paths.push(PathBuf::from(OsString::from_vec(decoded)));
    }
    if !paths
        .iter()
        .any(|path| path == std::path::Path::new("/oldroot"))
    {
        return Err("missing /oldroot mount".into());
    }
    paths.sort_by_key(|path| std::cmp::Reverse(path.as_os_str().len()));
    Ok(paths)
}

pub(super) fn unmount_old_root() -> Result<(), String> {
    let info = std::fs::read("/proc/self/mountinfo").map_err(|e| format!("read mountinfo: {e}"))?;
    let mut failures = Vec::new();
    for path in old_root_mounts(&info)? {
        if let Err(error) = unmount(&path, UnmountFlags::empty()) {
            failures.push(format!("unmount {}: {error}", path.display()));
        }
    }
    if failures.is_empty() {
        log::info!(target: "corium", "old root unmounted");
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricts_unmounts_and_orders_children_first() {
        let paths = old_root_mounts(b"1 0 0:1 / /oldroot rw - ext4 root rw\n2 1 0:2 / /oldroot/a\\040b rw - tmpfs tmpfs rw\n3 2 0:3 / /oldroot/a\\040b/child rw - tmpfs tmpfs rw\n4 0 0:4 / /oldroot-other rw - tmpfs tmpfs rw\n5 0 0:5 / /run rw - tmpfs tmpfs rw\n").unwrap();
        assert_eq!(
            paths,
            [
                PathBuf::from("/oldroot/a b/child"),
                PathBuf::from("/oldroot/a b"),
                PathBuf::from("/oldroot")
            ]
        );
        assert!(old_root_mounts(b"broken\n").is_err());
        assert!(old_root_mounts(b"1 0 0:1 / /elsewhere rw\n").is_err());
        assert!(old_root_mounts(b"1 0 0:1 / /oldroot/\\999 rw\n").is_err());
    }

    #[test]
    fn preserves_shutdown_action() {
        for (verb, expected) in [
            ("poweroff", RebootCommand::PowerOff),
            ("reboot", RebootCommand::Restart),
            ("halt", RebootCommand::Halt),
            ("kexec", RebootCommand::Kexec),
        ] {
            assert_eq!(action(Some(OsStr::new(verb))).unwrap(), expected);
        }
        assert!(action(None).is_err());
        assert!(action(Some(OsStr::new("unknown"))).is_err());
    }
}
