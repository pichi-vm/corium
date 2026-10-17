// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

#![cfg(target_os = "linux")]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn ordinary_process_refuses_init_duties() {
    // If the guard regresses, keep the test incapable of powering off the host.
    assert!(
        !rustix::process::geteuid().is_root(),
        "run tests without root privileges"
    );
    assert_eq!(corium::boot().unwrap_err(), "must run as PID 1");
    let mut child = Command::new(env!("CARGO_BIN_EXE_corium"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("ordinary-process invocation did not return");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "ERROR corium: message=\"must run as PID 1\"\n"
    );
}
