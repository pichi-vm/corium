// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::Harness;
use serde_json::{Value, json};

pub(crate) const READY: &str = "CORIUM-TEST-OK: systemd service started";
pub(crate) const PHASES: [&str; 6] = [
    "started",
    "modules",
    "discovered",
    "assembled",
    "mounted",
    "pivoted",
];

struct Guest(Child);
impl Drop for Guest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Debug)]
pub(crate) struct Boot {
    pub(crate) output: String,
    pub(crate) shutdown: Option<Value>,
    pub(crate) success: bool,
}

impl Boot {
    pub(crate) fn poweroff(&self) {
        let data = self.shutdown.as_ref().expect("missing QMP SHUTDOWN event");
        assert_eq!(data["guest"], true, "{data}");
        assert_eq!(data["reason"], "guest-shutdown", "{data}");
        assert!(self.success, "QEMU exited unsuccessfully");
        assert!(!self.output.contains("Kernel panic"), "kernel panic");
    }
    pub(crate) fn expect(&self, expected: &str) {
        self.poweroff();
        assert!(
            self.output.contains(expected),
            "missing {expected:?}\n{}",
            self.output
        );
        if expected != READY {
            assert!(
                !self.output.contains(READY),
                "unexpected successful handoff"
            );
        }
    }
}

// Read one byte at a time so a peer sending partial records cannot extend the
// overall deadline. QMP volume is tiny; throughput is irrelevant here.
fn record(stream: &mut UnixStream, deadline: Instant, log: &mut File) -> Option<Value> {
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("QMP deadline exceeded");
        stream.set_read_timeout(Some(remaining)).unwrap();
        let mut byte = [0];
        if stream.read(&mut byte).expect("read QMP before deadline") == 0 {
            assert!(bytes.is_empty(), "truncated QMP record");
            return None;
        }
        log.write_all(&byte).unwrap();
        if byte[0] == b'\n' {
            return Some(serde_json::from_slice(&bytes).expect("valid QMP JSON"));
        }
        bytes.push(byte[0]);
        assert!(bytes.len() < 1024 * 1024, "oversized QMP record");
    }
}

impl Harness {
    pub(crate) fn boot(
        &self,
        name: &str,
        initrd: &Path,
        disk: Option<&Path>,
        config: &str,
    ) -> Boot {
        self.boot_with_args(name, initrd, disk, config, "")
    }

    pub(crate) fn boot_with_args(
        &self,
        name: &str,
        initrd: &Path,
        disk: Option<&Path>,
        config: &str,
        options: &str,
    ) -> Boot {
        let mut bytes = fs::read(initrd).unwrap();
        if !config.is_empty() {
            bytes.resize(bytes.len().next_multiple_of(4), 0);
            let mut data = config.as_bytes().to_vec();
            data.push(0);
            data.resize(data.len().next_multiple_of(4), 0);
            let size = u32::try_from(data.len()).unwrap();
            let checksum = data
                .iter()
                .fold(0u32, |sum, b| sum.wrapping_add(u32::from(*b)));
            bytes.extend(data);
            bytes.extend(size.to_le_bytes());
            bytes.extend(checksum.to_le_bytes());
            bytes.extend(b"#BOOTCONFIG\n");
        }
        let archive = self.work.join(format!("{name}.initrd"));
        fs::write(&archive, bytes).unwrap();
        fs::write(self.work.join(format!("{name}.bootconfig")), config).unwrap();
        let socket_dir = tempfile::Builder::new()
            .prefix("corium-qmp-")
            .tempdir()
            .unwrap();
        let address = socket_dir.path().join("monitor");
        let listener = UnixListener::bind(&address).unwrap();
        listener.set_nonblocking(true).unwrap();
        let console_path = self.work.join(format!("{name}.log"));
        let console = File::create(&console_path).unwrap();
        let mut qmp_log = File::create(self.work.join(format!("{name}.qmp.log"))).unwrap();
        let mut command = Command::new("qemu-system-x86_64");
        command
            .args([
                "-enable-kvm",
                "-machine",
                "pc",
                "-cpu",
                "host",
                "-smp",
                "1",
                "-m",
                "768",
                "-nodefaults",
                "-nographic",
                "-serial",
                "stdio",
                "-monitor",
                "none",
                "-no-reboot",
                "-kernel",
            ])
            .arg(&self.kernel)
            .arg("-initrd")
            .arg(archive)
            .arg("-append")
            .arg(format!(
                "console=ttyS0 quiet panic=-1 systemd.show_status=false bootconfig {options}"
            ))
            .arg("-qmp")
            .arg(format!("unix:{}", address.display()))
            .stdin(Stdio::null())
            .stdout(console.try_clone().unwrap())
            .stderr(console);
        if let Some(disk) = disk {
            command.arg("-drive").arg(format!(
                "file={},format=raw,if=virtio,readonly=on,cache=writeback",
                disk.display()
            ));
        }
        fs::write(
            self.work.join(format!("{name}.command.txt")),
            format!("{command:?}\n"),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        let mut guest = Guest(command.spawn().expect("launch QEMU"));
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "QMP connection timeout; see {}",
                        console_path.display()
                    );
                    assert!(
                        guest.0.try_wait().unwrap().is_none(),
                        "QEMU exited before QMP connection"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("QMP accept: {error}"),
            }
        };
        assert!(
            record(&mut stream, deadline, &mut qmp_log)
                .unwrap()
                .get("QMP")
                .is_some()
        );
        stream
            .set_write_timeout(Some(deadline.saturating_duration_since(Instant::now())))
            .unwrap();
        stream
            .write_all(b"{\"execute\":\"qmp_capabilities\"}\n")
            .unwrap();
        let mut shutdown = None;
        while let Some(event) = record(&mut stream, deadline, &mut qmp_log) {
            if event["event"] == "SHUTDOWN" {
                shutdown = Some(event["data"].clone());
                break;
            }
            assert!(event.get("error").is_none(), "QMP error: {event}");
        }
        let exit_deadline = deadline.min(Instant::now() + Duration::from_secs(10));
        let status = loop {
            if let Some(status) = guest.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < exit_deadline, "guest exit timeout");
            thread::sleep(Duration::from_millis(5));
        };
        let output = String::from_utf8_lossy(&fs::read(console_path).unwrap()).into_owned();
        let result = Boot {
            output,
            shutdown,
            success: status.success(),
        };
        fs::write(
            self.work.join(format!("{name}.outcome.json")),
            serde_json::to_vec_pretty(&json!({
                "qemu_success": result.success, "shutdown": result.shutdown,
            }))
            .unwrap(),
        )
        .unwrap();
        result
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Timing {
    pub(crate) phase: String,
    pub(crate) elapsed: u64,
    pub(crate) duration: u64,
}

pub(crate) fn timings(output: &str, count: usize) -> Vec<Timing> {
    let mut result = Vec::new();
    let mut previous = 0;
    for line in output
        .lines()
        .filter(|line| line.contains("message=\"phase completed\""))
    {
        let (_, record) = line
            .split_once("INFO corium: ")
            .expect("info timing record");
        let normalized = format!("INFO corium: {record}");
        let fields: Vec<_> = normalized.split_whitespace().collect();
        assert_eq!(fields.len(), 7, "unexpected timing fields: {line}");
        assert_eq!(
            &fields[..4],
            &["INFO", "corium:", "message=\"phase", "completed\""]
        );
        let phase = fields[4]
            .strip_prefix("phase=\"")
            .unwrap()
            .strip_suffix('"')
            .unwrap();
        let elapsed: u64 = fields[5]
            .strip_prefix("elapsed_us=")
            .unwrap()
            .parse()
            .unwrap();
        let duration: u64 = fields[6]
            .strip_prefix("duration_us=")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(Some(&phase), PHASES.get(result.len()));
        assert!(
            elapsed >= previous && duration <= elapsed - previous,
            "invalid timing: {line}"
        );
        previous = elapsed;
        result.push(Timing {
            phase: phase.into(),
            elapsed,
            duration,
        });
    }
    assert_eq!(result.len(), count, "missing/extra phase records");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_records_allow_firmware_prefix_and_reject_invalid_sequences() {
        let line = "Booting from ROM..\u{1b}[2JINFO corium: message=\"phase completed\" phase=\"started\" elapsed_us=181 duration_us=181\n";
        assert_eq!(timings(line, 1)[0].elapsed, 181);
        assert!(std::panic::catch_unwind(|| timings(&format!("{line}{line}"), 2)).is_err());
        assert!(
            std::panic::catch_unwind(|| timings(
                &line.replace("duration_us=181", "duration_us=182"),
                1
            ))
            .is_err()
        );
        assert!(std::panic::catch_unwind(|| timings(line, 6)).is_err());
    }
}
