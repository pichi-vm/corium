// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::progress::Phase;

/// Corium entries from Linux's normalized /proc/bootconfig representation.
pub(crate) struct BootConfig<'a> {
    // None denotes multiple values; Corium's settings are all scalars.
    entries: Vec<(&'a str, Option<&'a str>)>,
}

impl<'a> BootConfig<'a> {
    pub(crate) fn parse(mut text: &'a str) -> Result<Self, String> {
        let mut entries = Vec::new();
        while !text.is_empty() {
            // Linux appends the bootloader command line as a comment trailer.
            // Its contents may include newlines, so the entire tail is opaque.
            if text.starts_with('#') {
                break;
            }
            if text.starts_with('\n') {
                text = text.split_once('\n').map_or("", |(_, rest)| rest);
                continue;
            }
            let (key, mut rest) = text
                .split_once(" = ")
                .ok_or("invalid /proc/bootconfig entry")?;
            if key.contains('\n') {
                return Err("invalid /proc/bootconfig key".into());
            }
            let mut scalar = None;
            let mut multiple = false;
            loop {
                let quote = rest
                    .get(..1)
                    .filter(|q| matches!(*q, "\"" | "'"))
                    .ok_or("invalid /proc/bootconfig value")?;
                let (value, tail) = rest[1..]
                    .split_once(quote)
                    .ok_or("unterminated /proc/bootconfig value")?;
                multiple |= scalar.replace(value).is_some();
                if let Some(next) = tail.strip_prefix(", ") {
                    rest = next;
                } else {
                    text = if tail.is_empty() {
                        tail
                    } else {
                        tail.strip_prefix('\n')
                            .ok_or("invalid /proc/bootconfig separator")?
                    };
                    break;
                }
            }
            // Parse complete values even outside our namespace: quoted values
            // can contain newlines, including text resembling Corium entries.
            if key == "corium" || key.starts_with("corium.") {
                entries.push((key, if multiple { None } else { scalar }));
            }
        }
        Ok(Self { entries })
    }

    pub(crate) fn check_keys(&self) -> Result<(), String> {
        for (key, _) in &self.entries {
            if !matches!(*key, "corium.root" | "corium.log" | "corium.halt") {
                return Err(format!("unknown bootconfig key: {key}"));
            }
        }
        Ok(())
    }

    fn option(&self, name: &str) -> Result<Option<&'a str>, String> {
        let mut entries = self.entries.iter().filter(|(key, _)| *key == name);
        let value = entries.next();
        if entries.next().is_some() {
            return Err(format!("duplicate {name} entries"));
        }
        value
            .map(|(_, value)| value.ok_or_else(|| format!("{name} requires one value")))
            .transpose()
    }

    /// Apply logging before validating the other settings so `off` suppresses
    /// their diagnostics too. Invalid logging retains the bootstrap filter.
    pub(crate) fn log_level(&self) -> Result<log::LevelFilter, String> {
        use log::LevelFilter;
        match self.option("corium.log")? {
            None | Some("error") => Ok(LevelFilter::Error),
            Some("off") => Ok(LevelFilter::Off),
            Some("warn") => Ok(LevelFilter::Warn),
            Some("info") => Ok(LevelFilter::Info),
            Some("debug") => Ok(LevelFilter::Debug),
            Some("trace") => Ok(LevelFilter::Trace),
            Some(value) => Err(format!("invalid corium.log value: {value}")),
        }
    }

    pub(crate) fn shutdown(&self) -> Result<Option<Phase>, String> {
        self.option("corium.halt")?.map(Phase::parse).transpose()
    }

    /// The tip's plain hexadecimal digest; Carapace validates its length.
    pub(crate) fn root_hash(&self) -> Result<Vec<u8>, String> {
        let hex = self.option("corium.root")?.ok_or("missing corium.root")?;
        if hex.is_empty()
            || !hex.len().is_multiple_of(2)
            || !hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(
                "corium.root must be nonempty, even-length hexadecimal without a prefix".into(),
            );
        }
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| e.to_string()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_optional_settings() {
        let config = BootConfig::parse("corium.root = \"00aBcD\"\n").unwrap();
        config.check_keys().unwrap();
        assert_eq!(config.root_hash().unwrap(), [0, 0xab, 0xcd]);
        assert_eq!(config.log_level().unwrap(), log::LevelFilter::Error);
        assert_eq!(config.shutdown().unwrap(), None);
        for length in [32, 64] {
            let text = format!("corium.root = \"{}\"\n", "A5".repeat(length));
            assert_eq!(
                BootConfig::parse(&text).unwrap().root_hash().unwrap(),
                vec![0xa5; length]
            );
        }
        for level in ["off", "error", "warn", "info", "debug", "trace"] {
            let text = format!("corium.log = \"{level}\"\ncorium.root = \"invalid\"\n");
            let config = BootConfig::parse(&text).unwrap();
            assert_eq!(
                config.log_level().unwrap().to_string().to_lowercase(),
                level
            );
            assert!(config.root_hash().is_err());
        }
        for (name, phase) in [
            ("started", Phase::Started),
            ("modules", Phase::Modules),
            ("discovered", Phase::Discovered),
            ("assembled", Phase::Assembled),
            ("mounted", Phase::Mounted),
            ("pivoted", Phase::Pivoted),
        ] {
            let text = format!("corium.halt = \"{name}\"\n");
            assert_eq!(
                BootConfig::parse(&text).unwrap().shutdown().unwrap(),
                Some(phase)
            );
        }
    }

    #[test]
    fn values_and_comments_cannot_inject_keys() {
        let text = concat!(
            "example = 'first line\ncorium.root = \"ff\"\nlast line', \"other\"\n",
            "other.corium.root = \"ff\"\n",
            "corium_extra.root = \"ff\"\n",
            "corium.root = \"ab\"\n",
            "# Parameters from bootloader:\n# quiet\ncorium.root = \"ff\"\n",
        );
        let config = BootConfig::parse(text).unwrap();
        config.check_keys().unwrap();
        assert_eq!(config.root_hash().unwrap(), [0xab]);
        assert!(
            BootConfig::parse("# Parameters from bootloader:\n# quiet\ncorium.root = \"ff\"\n")
                .unwrap()
                .root_hash()
                .is_err()
        );
        assert_eq!(
            BootConfig::parse("corium.root = 'ab'\n")
                .unwrap()
                .root_hash()
                .unwrap(),
            [0xab]
        );
    }

    #[test]
    fn rejects_missing_and_malformed_roots() {
        assert!(BootConfig::parse("").unwrap().root_hash().is_err());
        for value in [
            "", "abc", "0xabcd", "0XABCD", "gg", "a€", "é", "ab cd", "ab\ncd",
        ] {
            let text = format!("corium.root = \"{value}\"\n");
            assert!(
                BootConfig::parse(&text).unwrap().root_hash().is_err(),
                "{text:?}"
            );
        }
    }

    #[test]
    fn settings_require_unique_scalar_values() {
        for name in ["corium.root", "corium.log", "corium.halt"] {
            for text in [
                format!("{name} = \"ab\", \"cd\"\n"),
                format!("{name} = \"ab\"\n{name} = \"ab\"\n"),
            ] {
                assert!(
                    BootConfig::parse(&text).unwrap().option(name).is_err(),
                    "{text:?}"
                );
            }
        }
        for value in ["", "invalid", "INFO", "true"] {
            let text = format!("corium.log = \"{value}\"\ncorium.halt = \"{value}\"\n");
            let config = BootConfig::parse(&text).unwrap();
            assert!(config.log_level().is_err());
            assert!(config.shutdown().is_err());
        }
        for key in [
            "corium",
            "corium.rooot",
            "corium.root.extra",
            "corium.log.extra",
        ] {
            let text = format!("{key} = \"ab\"\n");
            assert!(BootConfig::parse(&text).unwrap().check_keys().is_err());
        }
    }

    #[test]
    fn rejects_truncated_or_malformed_records() {
        for text in [
            "corium.root",
            "corium.root = ab",
            "corium.root = \"ab",
            "corium.root = \"ab\" trailing\n",
            "corium.root = \"ab\", ",
            "bad\ncorium.root = \"ab\"\n",
        ] {
            assert!(BootConfig::parse(text).is_err(), "{text:?}");
        }
    }
}
