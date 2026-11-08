// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

mod metadata;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Load selected modules only after their dependencies are available.
pub(crate) fn load(root: &Path) -> Result<(), String> {
    let mut modules = BTreeMap::new();
    for path in Modules::collect(root).map_err(|e| format!("read {}: {e}", root.display()))? {
        let module = metadata::Module::try_from(path.as_path())
            .map_err(|e| format!("read module {}: {e}", path.display()))?;
        if modules.insert(module.name.clone(), module).is_some() {
            return Err(format!("duplicate module identity at {}", path.display()));
        }
    }
    let loaded = fs::read_dir("/sys/module")
        .map_err(|e| format!("read loaded modules: {e}"))?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<BTreeSet<_>>>()
        .map_err(|e| format!("read loaded modules: {e}"))?;
    let order = Modules(modules).order(loaded)?;
    // Resolve the complete graph before any kernel call: missing dependencies
    // and cycles must not leave a partially loaded selection.
    for module in order {
        log::debug!(path:% = module.path.display(); "loading module");
        let file = fs::File::open(&module.path)
            .map_err(|e| format!("open module {}: {e}", module.path.display()))?;
        match rustix::system::finit_module(&file, c"", 0) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(format!("load module {}: {error}", module.path.display())),
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Modules(BTreeMap<String, metadata::Module>);

impl Modules {
    fn collect(root: &Path) -> io::Result<Vec<PathBuf>> {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                files.extend(Self::collect(&path)?);
            } else if kind.is_file() && metadata::Module::name(&path).is_some() {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }

    fn order(mut self, mut available: BTreeSet<String>) -> Result<Vec<metadata::Module>, String> {
        self.0.retain(|name, _| !available.contains(name));
        for module in self.0.values() {
            for dependency in &module.dependencies {
                if !self.0.contains_key(dependency) && !available.contains(dependency) {
                    return Err(format!(
                        "module {} needs missing dependency {dependency}",
                        module.name
                    ));
                }
            }
        }
        let mut ordered = Vec::new();
        while !self.0.is_empty() {
            let ready: Vec<_> = self
                .0
                .iter()
                .filter(|(_, module)| module.dependencies.is_subset(&available))
                .map(|(name, _)| name.clone())
                .collect();
            if ready.is_empty() {
                return Err(format!(
                    "module dependency cycle: {}",
                    self.0.keys().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
            for name in ready {
                ordered.push(self.0.remove(&name).expect("selected existing module"));
                available.insert(name);
            }
        }
        Ok(ordered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(entries: &[(&str, &[&str])]) -> Modules {
        Modules(
            entries
                .iter()
                .map(|(name, dependencies)| {
                    (
                        (*name).into(),
                        metadata::Module {
                            name: (*name).into(),
                            path: format!("{name}.ko").into(),
                            dependencies: dependencies.iter().map(|name| (*name).into()).collect(),
                        },
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn sorts_transitive_dependencies_before_dependents() {
        let order = selection(&[("a", &["b", "c"]), ("b", &["c"]), ("c", &[])])
            .order(BTreeSet::new())
            .unwrap();
        assert_eq!(
            order.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["c", "b", "a"]
        );
    }

    #[test]
    fn accepts_dependencies_already_in_the_kernel() {
        let order = selection(&[("a", &["b"]), ("b", &["unneeded"])])
            .order(BTreeSet::from(["b".into()]))
            .unwrap();
        assert_eq!(order.len(), 1);
        assert_eq!(order[0].name, "a");
    }

    #[test]
    fn rejects_missing_dependencies_and_cycles_before_loading() {
        assert!(
            selection(&[("a", &["b"])])
                .order(BTreeSet::new())
                .unwrap_err()
                .contains("missing dependency b")
        );
        for entries in [
            vec![("a", &["a"][..])],
            vec![("a", &["b"][..]), ("b", &["a"][..])],
        ] {
            assert!(
                selection(&entries)
                    .order(BTreeSet::new())
                    .unwrap_err()
                    .contains("cycle")
            );
        }
        assert!(selection(&[]).order(BTreeSet::new()).unwrap().is_empty());
    }

    #[test]
    fn collects_modules_without_following_symlinks() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        fs::write(root.path().join("nested/a.ko.xz"), []).unwrap();
        fs::write(root.path().join("b.ko"), []).unwrap();
        fs::write(root.path().join("modules.dep"), []).unwrap();
        std::os::unix::fs::symlink("b.ko", root.path().join("link.ko")).unwrap();
        std::os::unix::fs::symlink(".", root.path().join("loop")).unwrap();
        assert_eq!(
            Modules::collect(root.path()).unwrap(),
            [root.path().join("b.ko"), root.path().join("nested/a.ko.xz")]
        );
    }
}
