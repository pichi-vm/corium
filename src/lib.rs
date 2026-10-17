// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Minimal Linux 7.0+ initramfs boot: establish a read-only Carapace root and
//! execute /sbin/init. All other runtime setup belongs to the real init.
//! Install [init_logging] before calling [boot].

#![cfg(target_os = "linux")]

mod boot;
mod cmdline;
mod discovery;
mod logging;
mod modules;
mod progress;

pub use boot::boot;
pub use logging::init_logging;
