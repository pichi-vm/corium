// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! corium — the pichi guest agent.
//!
//! The binary half (`src/main.rs`) runs as the initramfs `/init` (PID 1): it
//! parses `root.carapace=<root>` from the kernel cmdline, assembles the root
//! carapace, mounts it, and pivots. This library half carries the
//! cmdline-parse helper (unit-tested — the binary has `test = false`) plus the
//! console sentinels host-side boot tests match on.
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
}
