// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

// Shared by independent integration test executables; each uses a subset.
#![allow(dead_code)]

pub(crate) mod fixtures;
pub(crate) mod qemu;

use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

pub(crate) fn run(command: &mut Command) -> Output {
    let output = command
        .env("SOURCE_DATE_EPOCH", "1700000000")
        .env("E2FSPROGS_FAKE_TIME", "1700000000")
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("{command:?}: {e}"));
    assert!(
        output.status.success(),
        "{command:?}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

pub(crate) fn input(command: &mut Command, bytes: &[u8]) -> Output {
    let mut child = command
        .env("SOURCE_DATE_EPOCH", "1700000000")
        .env("E2FSPROGS_FAKE_TIME", "1700000000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Drain output concurrently with input to avoid pipe-capacity deadlocks.
    let mut stdin = child.stdin.take().unwrap();
    let data = bytes.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&data));
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

pub(crate) fn text(command: &mut Command) -> String {
    String::from_utf8(run(command).stdout).unwrap()
}

pub(crate) fn available(tool: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| p.join(tool).is_file()))
}

pub(crate) fn hash(path: &Path) -> String {
    let mut file = File::open(path).unwrap();
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 65536];
    loop {
        let n = file.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    format!("{:x}", digest.finalize())
}

#[derive(Debug)]
pub(crate) struct Harness {
    pub(crate) work: PathBuf,
    pub(crate) kernel: PathBuf,
    pub(crate) release: String,
    pub(crate) binary: PathBuf,
    pub(crate) config: String,
    pub(crate) modules: Vec<PathBuf>,
}

static HARNESS: OnceLock<Harness> = OnceLock::new();
pub(crate) fn harness() -> &'static Harness {
    HARNESS.get_or_init(Harness::new)
}

impl Harness {
    fn new() -> Self {
        assert_ne!(
            text(Command::new("id").arg("-u")).trim(),
            "0",
            "run the harness as an ordinary user"
        );
        for tool in [
            "cargo",
            "rustc",
            "qemu-system-x86_64",
            "mkfs.ext4",
            "debugfs",
            "e2fsck",
            "veritysetup",
            "sfdisk",
            "cpio",
            "modprobe",
            "ldd",
            "ldconfig",
            "sha256sum",
            "df",
            "lsblk",
            "git",
        ] {
            assert!(available(tool), "missing required tool: {tool}");
        }
        OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")
            .expect("read/write access to /dev/kvm required");
        let release = std::env::var("CORIUM_TEST_KERNEL_RELEASE")
            .unwrap_or_else(|_| text(Command::new("uname").arg("-r")).trim().into());
        assert!(
            release.split('.').next().unwrap().parse::<u32>().unwrap() >= 7,
            "Linux 7+ required"
        );
        let kernel = std::env::var_os("CORIUM_TEST_KERNEL").map_or_else(
            || PathBuf::from(format!("/boot/vmlinuz-{release}")),
            PathBuf::from,
        );
        File::open(&kernel).expect("readable guest kernel required");
        assert!(Path::new("/usr/lib/systemd/systemd").is_file());
        assert!(
            Path::new("/lib/modules").join(&release).is_dir(),
            "matching installed modules required"
        );
        let config_path = PathBuf::from(format!("/boot/config-{release}"));
        let config = fs::read_to_string(&config_path)
            .expect("matching readable kernel configuration required");
        let mut modules = std::collections::BTreeSet::new();
        let mut names = vec![
            "virtio_pci",
            "virtio_blk",
            "virtio_console",
            "dm_mod",
            "dm_zero",
            "dm_snapshot",
            "dm_verity",
            "ext4",
        ];
        if available("mksquashfs")
            && config
                .lines()
                .any(|line| matches!(line, "CONFIG_SQUASHFS=y" | "CONFIG_SQUASHFS=m"))
        {
            names.push("squashfs");
        }
        for name in names {
            for line in text(Command::new("modprobe").args([
                "--set-version",
                &release,
                "--show-depends",
                name,
            ]))
            .lines()
            {
                if let Some(rest) = line.strip_prefix("insmod ") {
                    let source = PathBuf::from(rest.split_whitespace().next().unwrap());
                    File::open(&source).expect("readable module required");
                    let tool = match source.extension().and_then(|s| s.to_str()) {
                        Some("xz") => Some("xz"),
                        Some("gz") => Some("gzip"),
                        Some("zst") => Some("zstd"),
                        Some("ko") => None,
                        _ => panic!("unsupported installed module format: {}", source.display()),
                    };
                    if let Some(tool) = tool {
                        assert!(
                            available(tool),
                            "missing required module decompressor: {tool}"
                        );
                    }
                    modules.insert(source);
                }
            }
        }
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let target = repo.join("target");
        fs::create_dir_all(&target).unwrap();
        let free: u64 = text(
            Command::new("df")
                .args(["-B1", "--output=avail"])
                .arg(&target),
        )
        .lines()
        .last()
        .unwrap()
        .trim()
        .parse()
        .unwrap();
        assert!(
            free >= 8 * 1024 * 1024 * 1024,
            "at least 8 GiB free artifact space required"
        );
        let work = tempfile::Builder::new()
            .prefix("boot-tests-")
            .tempdir_in(&target)
            .unwrap()
            .keep();
        println!("Artifacts: {}", work.display());
        // Explicit target-dir makes location independent of caller Cargo settings.
        run(Command::new("cargo")
            .current_dir(repo)
            .args([
                "+stable",
                "build",
                "--locked",
                "--release",
                "--target",
                "x86_64-unknown-linux-musl",
                "--target-dir",
            ])
            .arg(&target));
        let built = target.join("x86_64-unknown-linux-musl/release/corium");
        let binary = work.join("corium");
        fs::copy(built, &binary).unwrap();
        fs::copy(&config_path, work.join("kernel.config")).unwrap();
        fs::write(
            work.join("candidate.diff"),
            run(Command::new("git")
                .current_dir(repo)
                .args(["diff", "HEAD", "--binary"]))
            .stdout,
        )
        .unwrap();
        let untracked = text(Command::new("git").current_dir(repo).args([
            "ls-files",
            "--others",
            "--exclude-standard",
        ]));
        let untracked_hashes: Vec<_> = untracked
            .lines()
            .map(|p| json!({"path":p,"sha256":hash(&repo.join(p))}))
            .collect();
        let tools = [
            "rustc",
            "cargo",
            "qemu-system-x86_64",
            "veritysetup",
            "sfdisk",
            "cpio",
            "modprobe",
        ];
        let mut versions: Vec<_> = tools
            .iter()
            .map(|tool| json!({"tool":tool,"version":text(Command::new(tool).arg("--version"))}))
            .collect();
        for tool in ["mkfs.ext4", "debugfs", "e2fsck"] {
            let output = run(Command::new(tool).arg("-V"));
            versions.push(json!({"tool":tool,"version":String::from_utf8_lossy(&output.stderr)}));
        }
        for tool in ["xz", "gzip", "zstd", "mksquashfs"] {
            if available(tool) {
                let output = Command::new(tool)
                    .arg(if tool == "mksquashfs" {
                        "-version"
                    } else {
                        "--version"
                    })
                    .output()
                    .unwrap();
                versions.push(json!({"tool":tool,"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr)}));
            }
        }
        versions.push(json!({"tool":"guest rustc","version":text(Command::new("rustc").args(["+stable","--version"]))}));
        fs::write(work.join("environment.json"), serde_json::to_vec_pretty(&json!({
            "revision":text(Command::new("git").current_dir(repo).args(["rev-parse","HEAD"])),
            "status":text(Command::new("git").current_dir(repo).args(["status","--short"])),
            "untracked":untracked_hashes, "binary_sha256":hash(&binary),
            "lock_sha256":hash(&repo.join("Cargo.lock")), "kernel":kernel,
            "kernel_sha256":hash(&kernel), "release":release,
            "versions":versions, "cpu":fs::read_to_string("/proc/cpuinfo").unwrap(),
            "memory":fs::read_to_string("/proc/meminfo").unwrap(),
            "storage":text(Command::new("lsblk").args(["--json","-o","NAME,TYPE,SIZE,FSTYPE,MOUNTPOINTS,MODEL"])),
            "space":text(Command::new("df").arg("-B1").arg(&work)),
            "systemd":text(Command::new("/usr/lib/systemd/systemd").arg("--version")),
            "cache_policy":"fresh guest; repeated boots with warmed host cache; cache=writeback",
        })).unwrap()).unwrap();
        Self {
            work,
            kernel,
            release,
            binary,
            config,
            modules: modules.into_iter().collect(),
        }
    }

    pub(crate) fn squashfs(&self) -> bool {
        available("mksquashfs")
            && self
                .config
                .lines()
                .any(|line| matches!(line, "CONFIG_SQUASHFS=y" | "CONFIG_SQUASHFS=m"))
    }

    pub(crate) fn outcome(&self, name: &str, status: &str, detail: &str) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.work.join("cases.jsonl"))
            .unwrap();
        writeln!(
            file,
            "{}",
            json!({"name":name,"status":status,"detail":detail})
        )
        .unwrap();
        println!("{status} {name}: {detail}");
    }
}

pub(crate) struct Suite<'a> {
    harness: &'a Harness,
    name: &'a str,
}
impl Harness {
    pub(crate) fn suite<'a>(&'a self, name: &'a str) -> Suite<'a> {
        Suite {
            harness: self,
            name,
        }
    }
}
impl Drop for Suite<'_> {
    fn drop(&mut self) {
        let failed = std::thread::panicking();
        let cases = fs::read_to_string(self.harness.work.join("cases.jsonl")).unwrap_or_default();
        let mut passed = 0;
        let mut omitted = 0;
        for line in cases.lines() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                match value["status"].as_str() {
                    Some("PASS") => passed += 1,
                    Some("OMIT") => omitted += 1,
                    _ => {}
                }
            }
        }
        let summary = json!({"suite":self.name,"failed":failed,"invocation_cases_passed":passed,"invocation_cases_omitted":omitted});
        // Drop must not panic while unwinding from a failed assertion.
        let _ = fs::write(
            self.harness
                .work
                .join(format!("{}-summary.json", self.name)),
            format!("{summary}\n"),
        );
        eprintln!("{summary}");
    }
}
