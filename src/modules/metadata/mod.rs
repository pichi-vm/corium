// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use object::{Object as _, ObjectSection as _};
use std::collections::BTreeSet;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(super) struct Module {
    pub(super) name: String,
    pub(super) path: PathBuf,
    pub(super) dependencies: BTreeSet<String>,
}

impl Module {
    const LIMIT: u64 = 64 * 1024 * 1024;

    pub(super) fn name(path: &Path) -> Option<String> {
        let file = path.file_name()?.to_str()?;
        [".ko", ".ko.xz", ".ko.gz", ".ko.zst"]
            .into_iter()
            .find_map(|suffix| file.strip_suffix(suffix))
            .filter(|name| !name.is_empty())
            .map(|name| name.replace('-', "_"))
    }

    fn read(reader: impl io::Read) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        reader.take(Self::LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > Self::LIMIT {
            return Err(io::Error::other("module exceeds 64 MiB metadata limit"));
        }
        Ok(bytes)
    }

    fn dependencies(bytes: &[u8]) -> io::Result<BTreeSet<String>> {
        let elf = object::File::parse(bytes).map_err(io::Error::other)?;
        let section = elf
            .section_by_name(".modinfo")
            .ok_or_else(|| io::Error::other("module has no .modinfo section"))?;
        let data = section.data().map_err(io::Error::other)?;
        let mut dependencies = BTreeSet::new();
        for field in data.split(|byte| *byte == 0) {
            if let Some(value) = field.strip_prefix(b"depends=") {
                let value = std::str::from_utf8(value).map_err(io::Error::other)?;
                for name in value.split(',').filter(|name| !name.is_empty()) {
                    if !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    {
                        return Err(io::Error::other("invalid module dependency name"));
                    }
                    dependencies.insert(name.replace('-', "_"));
                }
            }
        }
        Ok(dependencies)
    }
}

impl TryFrom<&Path> for Module {
    type Error = io::Error;

    fn try_from(path: &Path) -> io::Result<Self> {
        let name = Self::name(path).ok_or_else(|| io::Error::other("invalid module filename"))?;
        if path.extension().is_none_or(|extension| extension != "ko") {
            return Err(io::Error::other(
                "compressed module: package plain .ko files with Arma",
            ));
        }
        let bytes = Self::read(std::fs::File::open(path)?)?;
        Ok(Self {
            name,
            path: path.to_owned(),
            dependencies: Self::dependencies(&bytes)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_names_and_normalization() {
        for name in [
            "dm-verity.ko",
            "dm-verity.ko.xz",
            "dm-verity.ko.gz",
            "dm-verity.ko.zst",
        ] {
            assert_eq!(Module::name(Path::new(name)).as_deref(), Some("dm_verity"));
        }
        for name in ["modules.dep", "a.ko.sig", "a.ko.backup", ".ko"] {
            assert!(Module::name(Path::new(name)).is_none());
        }
    }

    #[test]
    fn malformed_modules_fail_before_kernel_loading() {
        assert!(Module::dependencies(b"not an ELF").is_err());
    }

    #[test]
    fn reads_dependencies_from_elf_and_rejects_truncation() {
        let root = tempfile::tempdir().unwrap();
        let bytes = include_bytes!("../../../tests/data/module.ko");
        let path = root.path().join("dm-verity.ko");
        std::fs::write(&path, bytes).unwrap();
        let module = Module::try_from(path.as_path()).unwrap();
        assert_eq!(module.name, "dm_verity");
        assert_eq!(
            module.dependencies,
            BTreeSet::from(["dm_mod".into(), "reed_solomon".into()])
        );
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(Module::try_from(path.as_path()).is_err());
        for extension in ["ko.gz", "ko.xz", "ko.zst"] {
            assert!(
                Module::try_from(path.with_extension(extension).as_path())
                    .unwrap_err()
                    .to_string()
                    .contains("package plain .ko")
            );
        }
    }
}
