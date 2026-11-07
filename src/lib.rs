// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Minimal Linux 7.0+ initramfs boot: establish a verified Carapace root with
//! optional writable state and execute /sbin/init. The real init owns later setup.
//! Install [init_logging] before calling [boot].

#![cfg(target_os = "linux")]

mod boot;
mod bootconfig;
mod discovery;
mod disk;
mod logging;
mod modules;
mod progress;
mod state;

pub use boot::boot;
pub use logging::init_logging;
