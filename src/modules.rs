// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MODULE_INIT_COMPRESSED_FILE: i32 = 4;

/// Load regular module files recursively; an absent directory is an empty set.
pub(crate) fn load(root: &Path) -> Result<(), String> {
    let modules = collect(root).map_err(|e| format!("read modules {}: {e}", root.display()))?;
    log::debug!(count = modules.len(), path:% = root.display(); "module set collected");
    load_passes(modules, |path| {
        let file = fs::File::open(path)?;
        let flags = if path.extension().is_some_and(|x| x == "ko") {
            0
        } else {
            MODULE_INIT_COMPRESSED_FILE
        };
        match rustix::system::finit_module(&file, c"", flags) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => Ok(()),
            Err(error) => Err(error.into()),
        }
    })
}

fn collect(root: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files = Vec::new();
    collect_entries(entries, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_entries(entries: fs::ReadDir, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            collect_entries(fs::read_dir(path)?, files)?;
        } else if kind.is_file() && is_module(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn is_module(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| {
            [".ko", ".ko.xz", ".ko.gz", ".ko.zst"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
        })
}

fn load_passes(
    mut pending: Vec<PathBuf>,
    mut load: impl FnMut(&Path) -> io::Result<()>,
) -> Result<(), String> {
    let mut pass = 0;
    while !pending.is_empty() {
        pass += 1;
        let before = pending.len();
        let mut errors = Vec::new();
        pending.retain(|path| {
            log::trace!(pass, path:% = path.display(); "loading module");
            match load(path) {
            Ok(()) => {
                log::debug!(path:% = path.display(); "module loaded");
                false
            },
            Err(error) => {
                log::trace!(pass, path:% = path.display(), error:% = error; "module load attempt failed");
                errors.push(format!("{}: {error}", path.display()));
                true
            }
        }});
        if pending.len() == before {
            return Err(format!(
                "module loading made no progress: {}",
                errors.join("; ")
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_nested_regular_modules_without_following_symlinks() {
        let root = std::env::temp_dir().join(format!("corium-modules-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("nested/a.ko.xz"), []).unwrap();
        fs::write(root.join("b.ko"), []).unwrap();
        fs::write(root.join("modules.dep"), []).unwrap();
        std::os::unix::fs::symlink("b.ko", root.join("link.ko")).unwrap();
        std::os::unix::fs::symlink(".", root.join("loop")).unwrap();
        let found = collect(&root);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            found.unwrap(),
            [root.join("b.ko"), root.join("nested/a.ko.xz")]
        );
        assert_eq!(collect(&root).unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn dependent_modules_are_retried_but_successes_are_not() {
        let mut dependency_loaded = false;
        let mut attempts = Vec::new();
        load_passes(
            vec!["dependent.ko".into(), "dependency.ko".into()],
            |path| {
                attempts.push(path.to_owned());
                if path == Path::new("dependency.ko") {
                    dependency_loaded = true;
                    Ok(())
                } else if dependency_loaded {
                    Ok(())
                } else {
                    Err(io::Error::other("unresolved symbol"))
                }
            },
        )
        .unwrap();
        assert_eq!(
            attempts,
            vec![
                PathBuf::from("dependent.ko"),
                PathBuf::from("dependency.ko"),
                PathBuf::from("dependent.ko")
            ]
        );
    }

    #[test]
    fn no_progress_fails_with_module_diagnostics() {
        let mut calls = 0;
        let error = load_passes(vec!["broken.ko.xz".into()], |_| {
            calls += 1;
            Err(io::Error::other("invalid module"))
        })
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(error.contains("broken.ko.xz: invalid module"));
        load_passes(Vec::new(), |_| panic!("empty set should not load")).unwrap();
    }

    #[test]
    fn recognizes_only_supported_module_suffixes() {
        for name in ["a.ko", "a.ko.xz", "a.ko.gz", "a.ko.zst"] {
            assert!(is_module(Path::new(name)));
        }
        for name in ["modules.dep", "a.ko.sig", "a.ko.backup", "a.ko.xz.tmp"] {
            assert!(!is_module(Path::new(name)));
        }
    }
}
