// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! corium — the pichi guest agent.
//!
//! The binary half (`src/main.rs`) runs as the initramfs `/init` (PID 1): it
//! parses `root.carapace=<root>` from the kernel cmdline, assembles the root
//! carapace, mounts it, and pivots. This library half carries the
//! cmdline parsing and hash decoding (unit-tested — the binary has
//! `test = false`) plus the console sentinels host-side boot tests match on.
//!
//! MVP scope: the **initramfs phase** only (BUILD.md §8.1) — establish `/` from
//! the root carapace and pivot. The post-pivot contract enforcement (§9),
//! secondary-device mounting, and running under systemd are later work.

// Linux-only: the agent is device-mapper + mount + pivot, all Linux syscalls.
#![cfg(target_os = "linux")]

/// Printed once corium has mounted the root carapace and read back a file
/// from it. Host boot tests match this to confirm the root carapace assembled
/// and mounted inside the initramfs.
pub const CORIUM_ROOT_OK: &str = "CORIUM-ROOT-OK";

/// Printed just before corium pivots into the mounted root (or powers off, if
/// the root has no `/sbin/init`).
pub const CORIUM_PIVOT: &str = "CORIUM-PIVOT";

/// Extract the value of `root.carapace=<hex>` from a kernel cmdline. The root
/// is the carapace trust anchor (`rootₙ₋₁`), delivered on the cmdline (which
/// lives inside the measured PMI — BUILD.md §5.4). Returns the first match.
#[must_use]
pub fn parse_root_carapace(cmdline: &str) -> Option<&str> {
    cmdline
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("root.carapace="))
        .filter(|v| !v.is_empty())
}

/// Decode the kernel command line's root hash for Carapace's byte-based API.
/// Accepts either hex case and an optional `0x`/`0X` prefix, as the former
/// `carapace::attach` API did. Carapace validates the digest length.
pub fn decode_root_carapace(root: &str) -> Result<Vec<u8>, String> {
    let root = root.trim();
    let hex = root
        .strip_prefix("0x")
        .or_else(|| root.strip_prefix("0X"))
        .unwrap_or(root);
    if !hex.len().is_multiple_of(2) {
        return Err("root hash has an odd number of hex digits".into());
    }
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("root hash contains non-hex digits".into());
    }
    // ASCII validation above makes every two-byte boundary safe to slice.
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_root_carapace_token() {
        assert_eq!(
            parse_root_carapace("console=hvc0 root.carapace=deadbeef quiet"),
            Some("deadbeef")
        );
        assert_eq!(parse_root_carapace("console=hvc0 quiet"), None);
        assert_eq!(parse_root_carapace("root.carapace="), None);
    }

    #[test]
    fn decodes_root_hash_with_legacy_hex_forms() {
        for hex in ["00abcdef", "00ABCDEF", "0x00abcdef", " 0X00ABCDEF\n"] {
            assert_eq!(decode_root_carapace(hex).unwrap(), [0, 0xab, 0xcd, 0xef]);
        }
        for bytes in [32, 64] {
            assert_eq!(
                decode_root_carapace(&"a5".repeat(bytes)).unwrap(),
                vec![0xa5; bytes]
            );
        }
    }

    #[test]
    fn rejects_malformed_root_hex_without_panicking() {
        for hex in ["abc", "0xabc", "gg", "ab cd", "é", "a€"] {
            assert!(decode_root_carapace(hex).is_err(), "accepted {hex:?}");
        }
    }
}
