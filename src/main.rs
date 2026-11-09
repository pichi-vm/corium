// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Boot and shutdown PID 1.

mod shutdown;

fn main() -> std::process::ExitCode {
    corium::init_logging();
    // Never perform init duties (especially poweroff) in an ordinary process.
    if rustix::process::getpid() != rustix::process::Pid::INIT {
        log::error!(target: "corium", "must run as PID 1");
        return std::process::ExitCode::FAILURE;
    }
    let command =
        if std::env::args_os().next().as_deref() == Some(std::ffi::OsStr::new("/shutdown")) {
            if let Err(error) = corium::configure_logging() {
                log::error!(target: "corium", error:% = error; "configure shutdown logging");
            }
            match shutdown::action(std::env::args_os().nth(1).as_deref()) {
                Ok(command) => {
                    if let Err(error) = shutdown::unmount_old_root() {
                        log::error!(target: "corium", error:% = error; "shutdown incomplete");
                    }
                    command
                }
                Err(error) => {
                    log::error!(target: "corium", error:% = error; "invalid shutdown action");
                    rustix::system::RebootCommand::Halt
                }
            }
        } else {
            if let Err(error) = corium::boot() {
                log::error!(target: "corium", error:% = error; "boot failed");
            }
            rustix::system::RebootCommand::PowerOff
        };
    rustix::fs::sync();
    log::logger().flush();
    if let Err(error) = rustix::system::reboot(command) {
        log::error!(target: "corium", error:% = error; "shutdown action failed");
    }
    // PID 1 must not return, even if reboot fails.
    loop {
        std::thread::park();
    }
}
