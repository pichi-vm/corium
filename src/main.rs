// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! The initramfs PID 1. The mounted root is its only handoff resource.

fn main() -> std::process::ExitCode {
    corium::init_logging();
    // Never perform init duties (especially poweroff) in an ordinary process.
    if rustix::process::getpid() != rustix::process::Pid::INIT {
        log::error!(target: "corium", "must run as PID 1");
        return std::process::ExitCode::FAILURE;
    }
    if let Err(error) = corium::boot() {
        log::error!(target: "corium", error:% = error; "boot failed");
    }
    rustix::fs::sync();
    log::logger().flush();
    let _ = rustix::system::reboot(rustix::system::RebootCommand::PowerOff);
    // PID 1 must not return, even if reboot fails.
    loop {
        std::thread::park();
    }
}
