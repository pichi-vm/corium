// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::progress::Phase;

/// Parse logging separately so a valid filter applies even if other options
/// fail validation, regardless of token order. Invalid/duplicate log options
/// retain the bootstrap error filter.
pub(crate) fn log_level(cmdline: &str) -> Result<log::LevelFilter, String> {
    use log::LevelFilter;
    match option(cmdline, "corium.log")? {
        None | Some("error") => Ok(LevelFilter::Error),
        Some("off") => Ok(LevelFilter::Off),
        Some("warn") => Ok(LevelFilter::Warn),
        Some("info") => Ok(LevelFilter::Info),
        Some("debug") => Ok(LevelFilter::Debug),
        Some("trace") => Ok(LevelFilter::Trace),
        Some(value) => Err(format!("invalid corium.log value: {value}")),
    }
}

/// Parse the optional successful-phase power-off boundary.
pub(crate) fn shutdown(cmdline: &str) -> Result<Option<Phase>, String> {
    option(cmdline, "corium.halt")?
        .map(Phase::parse)
        .transpose()
}

fn option<'a>(cmdline: &'a str, name: &str) -> Result<Option<&'a str>, String> {
    let mut values = cmdline.split_whitespace().filter_map(|token| {
        if token == name {
            Some("")
        } else {
            token.strip_prefix(name)?.strip_prefix('=')
        }
    });
    let value = values.next();
    if values.next().is_some() {
        return Err(format!("duplicate {name} arguments"));
    }
    Ok(value)
}

/// Exactly one corium=<plain hex> token. Digest length is validated by Carapace.
pub(crate) fn root_hash(cmdline: &str) -> Result<Vec<u8>, String> {
    let hex = option(cmdline, "corium")?.ok_or("missing corium=<root-hash>")?;
    if hex.is_empty() || !hex.len().is_multiple_of(2) || !hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("corium must be nonempty, even-length hexadecimal without a prefix".into());
    }
    // ASCII validation ensures these byte boundaries are valid UTF-8 boundaries.
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_levels_and_shutdown_points_are_exact() {
        for value in ["off", "error", "warn", "info", "debug", "trace"] {
            assert_eq!(
                log_level(&format!("corium.log={value}"))
                    .unwrap()
                    .to_string()
                    .to_lowercase(),
                value
            );
        }
        assert_eq!(log_level("").unwrap(), log::LevelFilter::Error);
        assert_eq!(shutdown("").unwrap(), None);
        for (value, phase) in [
            ("started", Phase::Started),
            ("modules", Phase::Modules),
            ("discovered", Phase::Discovered),
            ("assembled", Phase::Assembled),
            ("mounted", Phase::Mounted),
            ("pivoted", Phase::Pivoted),
        ] {
            assert_eq!(
                shutdown(&format!("corium.halt={value}")).unwrap(),
                Some(phase)
            );
        }
        for option in ["corium.log", "corium.halt"] {
            for value in ["", "invalid", "INFO", "true"] {
                assert!(if option == "corium.log" {
                    log_level(&format!("{option}={value}")).is_err()
                } else {
                    shutdown(&format!("{option}={value}")).is_err()
                });
            }
        }
        assert!(log_level("corium.log").is_err());
        assert!(shutdown("corium.halt").is_err());
        assert!(
            log_level("corium.log=off corium.log=info")
                .unwrap_err()
                .contains("duplicate")
        );
        assert!(
            shutdown("corium.halt=started corium.halt")
                .unwrap_err()
                .contains("duplicate")
        );
        assert_eq!(
            root_hash("corium.log=off corium=ab corium.halt=started").unwrap(),
            [0xab]
        );
        assert_eq!(
            log_level("other.corium.log=trace corium.logger=debug").unwrap(),
            log::LevelFilter::Error
        );
    }

    #[test]
    fn accepts_plain_hex_in_either_case() {
        assert_eq!(
            root_hash("console=ttyS0 corium=00aBcD quiet").unwrap(),
            [0, 0xab, 0xcd]
        );
        for length in [32, 64] {
            assert_eq!(
                root_hash(&format!("corium={}", "A5".repeat(length))).unwrap(),
                vec![0xa5; length]
            );
        }
    }

    #[test]
    fn rejects_missing_malformed_and_legacy_arguments() {
        for cmdline in [
            "",
            "root.carapace=abcd",
            "carapacehash=abcd",
            "corium",
            "corium=",
            "corium=abc",
            "corium=0xabcd",
            "corium=0XABCD",
            "corium=gg",
            "corium=a€",
            "corium=é",
            "other.corium=abcd",
            "corium=\"abcd\"",
        ] {
            assert!(root_hash(cmdline).is_err(), "accepted {cmdline:?}");
        }
    }

    #[test]
    fn rejects_all_duplicate_forms() {
        for cmdline in [
            "corium=abcd corium=abcd",
            "corium= corium=abcd",
            "corium=abcd corium=",
            "corium corium=abcd",
        ] {
            assert_eq!(
                root_hash(cmdline).unwrap_err(),
                "duplicate corium arguments"
            );
        }
    }
}
