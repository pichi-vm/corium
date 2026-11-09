// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

#![cfg(target_os = "linux")]
mod support;

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use support::qemu::{PHASES, READY, timings};
use support::{fixtures as f, harness};

#[test]
#[ignore = "requires QEMU/KVM, Linux 7+ kernel/modules and image tools"]
fn init_arguments() {
    let h = harness();
    let _suite = h.suite("init-arguments");
    let root = f::argument_root(h);
    let disk = f::base(h, &root, "arguments", false);
    let initrd = f::initramfs(h, "arguments", false, false, "none");
    let boot = h.boot_with_args(
        "init-arguments",
        &initrd,
        Some(&disk.path),
        &format!("corium.root={}; init.extra=\"bootconfig value\"", disk.root),
        "CORIUM_TEST_ENV=\"value with spaces\" -- first \"two words\" \"\" key=value first -x é",
    );
    boot.expect(READY);
    let arguments: Vec<_> = boot
        .output
        .lines()
        .filter_map(|line| line.strip_prefix("CORIUM-INIT-ARG:"))
        .collect();
    assert_eq!(
        arguments,
        [
            "<first>",
            "<two words>",
            "<>",
            "<key=value>",
            "<first>",
            "<-x>",
            "<é>",
            "<extra=bootconfig value>"
        ]
    );
    assert!(boot.output.contains("CORIUM-INIT-ARGV0:</sbin/init>"));
    assert!(boot.output.contains("CORIUM-INIT-ENV:<value with spaces>"));
    h.outcome("init-arguments", "PASS", "");
}

#[test]
#[ignore = "requires QEMU/KVM, Linux 7+ kernel/modules and image tools"]
fn mounted_root_can_be_a_snapshot_origin() {
    let h = harness();
    let _suite = h.suite("shared-root");
    let root = f::shared_root(h);
    let initrd = f::initramfs(h, "shared-root", false, false, "none");
    for name in ["shared-root", "naked-shared-root"] {
        let disk = if name == "shared-root" {
            f::base(h, &root, name, false)
        } else {
            f::naked(h, &root, name)
        };
        let boot = h.boot(
            name,
            &initrd,
            Some(&disk.path),
            &format!("corium.root={}", disk.root),
        );
        boot.expect(READY);
        assert!(boot.output.contains("CORIUM-SHARED-ROOT-OK"));
        h.outcome(name, "PASS", "mounted root remains a reusable origin");
    }
}

#[test]
#[ignore = "requires QEMU/KVM, Linux 7+ kernel/modules and image tools"]
fn partitioned_roots() {
    let h = harness();
    let _suite = h.suite("partitioned-roots");
    let root = f::root(h, "partitioned-root-tree");
    let initrd = f::initramfs(h, "partitioned-roots", false, false, "none");
    for (name, expected) in [
        ("root-with-esp", READY),
        (
            "wrong-root-architecture",
            "no discoverable root for partition type",
        ),
        ("ambiguous-root", "multiple discoverable root partitions"),
        ("corrupt-backup-gpt", "create root mount view:"),
    ] {
        let disk = f::partition_case(h, &root, name);
        let boot = h.boot(
            name,
            &initrd,
            Some(&disk.path),
            &format!("corium.root={}; corium.log=debug", disk.root),
        );
        boot.expect(expected);
        if expected == READY {
            assert!(boot.output.contains("root partition selected"));
        } else {
            assert!(!boot.output.contains(READY));
            assert!(!boot.output.contains("waiting for root partitions"));
        }
        h.outcome(name, "PASS", "");
    }
}

#[test]
#[ignore = "requires QEMU/KVM, Linux 7+ kernel/modules and image tools"]
fn boot_contract() {
    let h = harness();
    let _suite = h.suite("boot-contract");
    let root = f::root(h, "small-root");
    let disk = f::base(h, &root, "small", false);
    let initrd = f::initramfs(h, "plain", false, false, "none");
    let config = format!("corium.root={}", disk.root);
    let run = |name: &str, options: &str, expected: &str| {
        let boot = h.boot(name, &initrd, Some(&disk.path), options);
        boot.expect(expected);
        h.outcome(name, "PASS", "");
        boot.output
    };
    let boot = h.boot_with_args(
        "systemd-handoff",
        &initrd,
        Some(&disk.path),
        &config,
        "loglevel=7",
    );
    boot.expect(READY);
    let output = boot.output;
    assert!(
        output.contains("EXT4-fs"),
        "kernel diagnostics must be enabled"
    );
    for rejected in ["couldn't mount as ext3", "couldn't mount as ext2"] {
        assert!(
            !output.contains(rejected),
            "unexpected kernel diagnostic: {rejected}\n{output}"
        );
    }
    assert!(!output.contains("Unknown symbol"), "{output}");
    h.outcome("systemd-handoff", "PASS", "dependency-ordered modules");
    assert!(!output.contains("corium:"));
    let missing_mount = f::root(h, "missing-api-mountpoint-root");
    std::fs::remove_dir(missing_mount.join("sys")).unwrap();
    let missing_mount = f::base(h, &missing_mount, "missing-api-mountpoint", false);
    let boot = h.boot(
        "missing-api-mountpoint",
        &initrd,
        Some(&missing_mount.path),
        &format!("corium.root={}", missing_mount.root),
    );
    boot.expect("move /sys into root");
    h.outcome(
        "missing-api-mountpoint",
        "PASS",
        "missing mountpoint fails before pivot",
    );
    let panic = h.boot_with_args(
        "panic-negative-control",
        &initrd,
        None,
        "corium.log=off",
        "rdinit=/nonexistent",
    );
    assert!(panic.output.contains("Kernel panic"));
    assert!(
        panic
            .shutdown
            .as_ref()
            .is_none_or(|s| s["guest"] != true || s["reason"] != "guest-shutdown")
    );
    h.outcome("panic-negative-control", "PASS", "panic rejected");
    for (index, phase) in PHASES.iter().enumerate() {
        let output = run(
            &format!("halt-{phase}"),
            &format!("{config}; corium.log=info; corium.halt={phase}"),
            &format!("message=\"shutdown requested\" phase=\"{phase}\""),
        );
        timings(&output, index + 1);
        assert!(!output.contains(READY));
        for unexpected in ["ERROR corium", "DEBUG corium", "TRACE corium"] {
            assert!(!output.contains(unexpected));
        }
    }
    for level in ["info", "debug", "trace"] {
        let output = run(
            &format!("logging-{level}"),
            &format!("{config}; corium.log={level}"),
            READY,
        );
        timings(&output, PHASES.len());
        assert_eq!(output.contains("DEBUG corium"), level != "info");
        assert_eq!(output.contains("TRACE corium"), level == "trace");
    }
    for (name, options) in [
        ("off-fatal", "corium.log=off; corium.root=invalid".into()),
        (
            "off-halt-invalid",
            format!("{config}; corium.halt=bad; corium.log=off"),
        ),
        ("silent-halt", format!("{config}; corium.halt=started")),
        (
            "off-halt",
            format!("{config}; corium.halt=started; corium.log=off"),
        ),
        (
            "warn-halt",
            format!("{config}; corium.halt=started; corium.log=warn"),
        ),
    ] {
        let output = run(name, &options, "");
        assert!(!output.contains("corium:"));
    }
    for (name, options, expected) in [
        (
            "invalid-log",
            "corium.log=invalid".into(),
            "invalid corium.log value",
        ),
        (
            "array-log",
            "corium.log=off,info".into(),
            "corium.log requires one value",
        ),
        (
            "invalid-halt",
            format!("{config}; corium.halt=bad"),
            "invalid corium.halt value",
        ),
        (
            "array-halt",
            format!("{config}; corium.halt=started,modules"),
            "corium.halt requires one value",
        ),
        (
            "failure-before-halt",
            "corium.root=invalid; corium.log=info; corium.halt=started".into(),
            "corium.root must be nonempty",
        ),
        (
            "array-root",
            format!("{config},{}", disk.root),
            "corium.root requires one value",
        ),
        ("missing-root", String::new(), "missing corium.root"),
        (
            "malformed",
            format!("corium.root=0x{}", disk.root),
            "corium.root must be nonempty",
        ),
        (
            "unknown-key",
            format!("{config}; corium.rooot=ab"),
            "unknown bootconfig key: corium.rooot",
        ),
        (
            "root-subkey",
            format!("{config}; corium.root.extra=ab"),
            "unknown bootconfig key: corium.root.extra",
        ),
        (
            "malformed-document",
            "corium { root =".into(),
            "missing corium.root",
        ),
    ] {
        let output = run(name, &options, expected);
        assert!(output.contains("ERROR corium: message=\"boot failed\""));
        assert!(!output.contains("message=\"shutdown requested\""));
    }
    let native = format!(
        "corium {{ root=00; log=off; }}\ncorium.root := {};\ncorium.log := info;\n\
         example = 'first line\ncorium.root = \"ff\"\nlast line';\nexample += more;\n\
         kernel.panic = -1;",
        disk.root,
    );
    let output = run("native-bootconfig", &native, READY);
    timings(&output, PHASES.len());
    h.boot_with_args(
        "bootconfig-required",
        &initrd,
        Some(&disk.path),
        "",
        &format!("corium={}", disk.root),
    )
    .expect("missing corium.root");
    h.outcome("bootconfig-required", "PASS", "");
    h.boot(
        "incorrect-root",
        &initrd,
        Some(&disk.path),
        &format!("corium.root={}", "ff".repeat(32)),
    )
    .expect("waiting for root partitions");
    h.outcome("incorrect-root", "PASS", "");
    h.boot("discovery-timeout", &initrd, None, &config)
        .expect("waiting for root partitions");
    h.outcome("discovery-timeout", "PASS", "");
    for (name, empty, bad, expected) in [
        ("bad-module", false, true, "read module"),
        ("absent-modules", true, false, "waiting for root partitions"),
    ] {
        let archive = f::initramfs(h, name, empty, bad, "none");
        h.boot(
            name,
            &archive,
            if empty { None } else { Some(&disk.path) },
            &config,
        )
        .expect(expected);
        h.outcome(name, "PASS", "");
    }
    for (compression, tool) in [("xz", "xz"), ("gz", "gzip"), ("zst", "zstd")] {
        let name = format!("modules-{compression}");
        if !support::available(tool) {
            h.outcome(&name, "OMIT", "compression tool not installed");
            continue;
        }
        let archive = f::initramfs(h, &name, false, false, compression);
        h.boot(&name, &archive, Some(&disk.path), &config)
            .expect("package plain .ko files with Arma");
        h.outcome(&name, "PASS", "compressed input rejected before loading");
    }
    for (name, offset, bytes, expected) in [
        (
            "invalid-superblock",
            disk.offsets[0].1,
            b"BADMAGIC".as_slice(),
            "discover root:",
        ),
        (
            "tampered-root-data",
            disk.offsets[0].0 + 2 * 4096 + 64,
            b"X".as_slice(),
            "create root mount view:",
        ),
    ] {
        let path = h.work.join(format!("{name}.gpt"));
        fs::copy(&disk.path, &path).unwrap();
        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(bytes).unwrap();
        let result = h.boot(name, &initrd, Some(&path), &config);
        result.expect(expected);
        assert!(!result.output.contains("waiting for root partitions"));
        h.outcome(name, "PASS", "");
    }
    if h.squashfs() {
        let squash = f::base(h, &root, "squash", true);
        h.boot(
            "squashfs-handoff",
            &initrd,
            Some(&squash.path),
            &format!("corium.root={}", squash.root),
        )
        .expect(READY);
        h.outcome("squashfs-handoff", "PASS", "");
    } else {
        h.outcome(
            "squashfs-handoff",
            "OMIT",
            "mksquashfs or guest squashfs driver unavailable",
        );
    }
    fs::remove_file(root.join("sbin/init")).unwrap();
    let missing = f::base(h, &root, "missing-init", false);
    h.boot(
        "exec-failure",
        &initrd,
        Some(&missing.path),
        &format!("corium.root={}", missing.root),
    )
    .expect("exec /sbin/init:");
    h.outcome("exec-failure", "PASS", "");
}

#[test]
#[ignore = "requires QEMU/KVM, Linux 7+ kernel/modules and image tools"]
fn xfs_and_btrfs_roots() {
    let h = harness();
    let _suite = h.suite("filesystem-roots");
    let root = f::root(h, "other-filesystem-root");
    let initrd = f::initramfs(h, "filesystem-roots", false, false, "none");
    for fstype in ["xfs", "btrfs"] {
        let disk = h.filesystem_root(&root, fstype);
        let boot = h.boot_with_args(
            &format!("{fstype}-handoff"),
            &initrd,
            Some(&disk.path),
            &format!("corium.root={}; corium.log=debug", disk.root),
            "loglevel=7",
        );
        boot.expect(READY);
        assert!(
            boot.output.contains(&format!("filesystem=\"{fstype}\"")),
            "{}",
            boot.output
        );
        for rejected in ["couldn't mount as ext3", "couldn't mount as ext2"] {
            assert!(
                !boot.output.contains(rejected),
                "{rejected}\n{}",
                boot.output
            );
        }
        h.outcome(
            &format!("{fstype}-handoff"),
            "PASS",
            "detected root, mounted read-only and executed systemd",
        );
    }
}

#[test]
#[ignore = "requires QEMU/KVM, Linux 7+ kernel/modules and image tools"]
fn layered_roots() {
    let h = harness();
    let _suite = h.suite("layered-roots");
    let initrd = f::initramfs(h, "layers", false, false, "none");
    let (small, scutes) = f::layered(h, "three-scutes", 3);
    h.boot(
        "three-scutes",
        &initrd,
        Some(&small.path),
        &format!("corium.root={}", small.root),
    )
    .expect(READY);
    h.outcome("three-scutes", "PASS", "");
    let missing = f::pack(&h.work, "missing-parent", &scutes, Some(1));
    h.boot(
        "missing-parent",
        &initrd,
        Some(&missing.path),
        &format!("corium.root={}", missing.root),
    )
    .expect("waiting for root partitions");
    h.outcome("missing-parent", "PASS", "");
    // The intermediate snapshot header is necessarily read during assembly.
    let path = h.work.join("corrupt-parent.gpt");
    fs::copy(&small.path, &path).unwrap();
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(small.offsets[1].0)).unwrap();
    file.write_all(b"BAD!").unwrap();
    h.boot(
        "corrupt-parent",
        &initrd,
        Some(&path),
        &format!("corium.root={}", small.root),
    )
    .expect("assemble root:");
    h.outcome("corrupt-parent", "PASS", "");
}

#[test]
#[ignore = "requires QEMU/KVM, Linux 7+ kernel/modules and large image tools"]
fn large_root() {
    let h = harness();
    let _suite = h.suite("large-root");
    let initrd = f::initramfs(h, "large", false, false, "none");
    let (large, _) = f::layered(h, "large", 50);
    let result = h.boot(
        "large-handoff",
        &initrd,
        Some(&large.path),
        &format!("corium.root={}", large.root),
    );
    result.expect(READY);
    assert!(!result.output.contains("corium:"));
    h.outcome("large-handoff", "PASS", "");
}

#[test]
#[ignore = "requires QEMU/KVM, Linux 7+ kernel/modules and image tools"]
fn preformatted_var() {
    let h = harness();
    let _suite = h.suite("preformatted-var");
    let root = f::persistent_var_root(h, "persistent-var-root");
    let disk = f::base(h, &root, "persistent-var", false);
    let initrd = f::initramfs(h, "persistent-var", false, false, "none");
    let config = format!("corium.root={}", disk.root);
    let original = support::hash(&disk.path);
    for sector in [512, 4096] {
        let state = f::existing_state(h, sector);
        for (boot, marker) in [(1, "NEW"), (2, "RETAINED"), (3, "RETAINED")] {
            let name = format!("state-{sector}-{boot}");
            let result = h.boot_with_storage(
                &name,
                &initrd,
                Some(&disk.path),
                &config,
                "",
                &[(&state, sector)],
            );
            result.expect(READY);
            assert!(result.output.contains(&format!("CORIUM-STATE-{marker}")));
            h.outcome(&name, "PASS", marker);
        }
    }
    let first = h.work.join("state-512.gpt");
    let second = h.work.join("state-4096.gpt");
    let hashes = [support::hash(&first), support::hash(&second)];
    let ambiguous = h.boot_with_storage(
        "ambiguous-state",
        &initrd,
        Some(&disk.path),
        &config,
        "",
        &[(&first, 512), (&second, 4096)],
    );
    ambiguous.expect("exactly one writable whole virtio disk; found 2");
    h.outcome("ambiguous-state", "PASS", "");
    let ordinary_root = f::root(h, "ordinary-root");
    let ordinary = f::base(h, &ordinary_root, "ordinary", false);
    let no_state = h.boot(
        "no-state-disk",
        &initrd,
        Some(&ordinary.path),
        &format!("corium.root={}", ordinary.root),
    );
    no_state.expect(READY);
    assert_eq!([support::hash(&first), support::hash(&second)], hashes);
    h.outcome("no-state-disk", "PASS", "immutable boot without state");
    let unknown = h.work.join("unknown-state.raw");
    let mut file = fs::File::create(&unknown).unwrap();
    file.set_len(8 * 1024 * 1024).unwrap();
    file.write_all(b"user data").unwrap();
    drop(file);
    let before = support::hash(&unknown);
    let result = h.boot_with_storage(
        "unknown-state",
        &initrd,
        Some(&disk.path),
        &config,
        "",
        &[(&unknown, 512)],
    );
    result.expect("prepare persistent state:");
    assert_eq!(support::hash(&unknown), before);
    h.outcome("unknown-state", "PASS", "unrecognized disk unchanged");
    // A recognized GPT must not authorize replacing a damaged filesystem.
    let mut damaged = OpenOptions::new().write(true).open(&first).unwrap();
    damaged
        .seek(SeekFrom::Start(1024 * 1024 + 1024 + 56))
        .unwrap();
    damaged.write_all(&[0, 0]).unwrap();
    drop(damaged);
    let before = support::hash(&first);
    let result = h.boot_with_storage(
        "damaged-filesystem",
        &initrd,
        Some(&disk.path),
        &config,
        "",
        &[(&first, 512)],
    );
    result.expect("mount state as ext4:");
    assert_eq!(support::hash(&first), before);
    h.outcome("damaged-filesystem", "PASS", "filesystem not reformatted");
    assert_eq!(support::hash(&disk.path), original);
}

#[test]
#[ignore = "requires QEMU/KVM, Linux 7+ kernel/modules and image tools"]
fn initialize_state() {
    let h = harness();
    let _suite = h.suite("initialize-state");
    let root = f::persistent_var_root(h, "initial-state-root");
    f::executable("/usr/sbin/mkfs.ext4", &root);
    let disk = f::base(h, &root, "initial-state", false);
    let initrd = f::initramfs(h, "initial-state", false, false, "none");
    let config = format!("corium.root={}", disk.root);
    let original = support::hash(&disk.path);
    for sector in [512, 4096] {
        let state = h.work.join(format!("blank-state-{sector}.raw"));
        fs::File::create(&state)
            .unwrap()
            .set_len(64 * 1024 * 1024)
            .unwrap();
        for (number, marker) in [(1, "NEW"), (2, "RETAINED")] {
            let name = format!("initialize-{sector}-{number}");
            let result = h.boot_with_storage(
                &name,
                &initrd,
                Some(&disk.path),
                &config,
                "",
                &[(&state, sector)],
            );
            result.expect(READY);
            assert!(result.output.contains(&format!("CORIUM-STATE-{marker}")));
            h.outcome(&name, "PASS", marker);
        }
    }
    assert_eq!(support::hash(&disk.path), original);
    let root = f::persistent_var_root(h, "failed-formatter-root");
    for failure in ["missing", "failed"] {
        if failure == "failed" {
            use std::os::unix::fs::PermissionsExt;
            fs::create_dir_all(root.join("usr/sbin")).unwrap();
            let formatter = root.join("usr/sbin/mkfs.ext4");
            fs::write(&formatter, "#!/usr/bin/sh\nexit 7\n").unwrap();
            fs::set_permissions(formatter, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let disk = f::base(h, &root, &format!("formatter-{failure}"), false);
        let config = format!("corium.root={}", disk.root);
        let state = h.work.join(format!("formatter-{failure}.raw"));
        fs::File::create(&state)
            .unwrap()
            .set_len(64 * 1024 * 1024)
            .unwrap();
        let result = h.boot_with_storage(
            &format!("formatter-{failure}"),
            &initrd,
            Some(&disk.path),
            &config,
            "",
            &[(&state, 512)],
        );
        result.expect(if failure == "missing" {
            "execute mkfs.ext4:"
        } else {
            "mkfs.ext4 failed:"
        });
        let before = support::hash(&state);
        let retry = h.boot_with_storage(
            &format!("formatter-{failure}-retry"),
            &initrd,
            Some(&disk.path),
            &config,
            "",
            &[(&state, 512)],
        );
        retry.expect("mount state as ext4:");
        assert_eq!(support::hash(&state), before);
        h.outcome(
            &format!("formatter-{failure}"),
            "PASS",
            "failed initialization not reformatted",
        );
    }
}

#[test]
#[ignore = "requires QEMU/KVM, Linux 7+ kernel/modules and image tools"]
fn bind_policy() {
    let h = harness();
    let _suite = h.suite("state-root");
    let root = f::bind_policy_root(h);
    let disk = f::base(h, &root, "state-base", false);
    let original = support::hash(&disk.path);
    let initrd = f::initramfs(h, "state", false, false, "none");
    let config = format!("corium.root={}\ncorium.log=info", disk.root);
    let no_state = h.boot(
        "state-halt-no-disk",
        &initrd,
        Some(&disk.path),
        &format!("{config}\ncorium.halt=state"),
    );
    no_state.expect("phase=\"state\"");
    assert!(!no_state.output.contains(READY));
    h.outcome(
        "state-halt-no-disk",
        "PASS",
        "no disk completes state phase without mounting",
    );
    for sector in [512, 4096] {
        let path = h.work.join(format!("state-{sector}.raw"));
        fs::File::create(&path)
            .unwrap()
            .set_len(64 * 1024 * 1024)
            .unwrap();
        let halted = h.boot_with_storage(
            &format!("state-{sector}-halt"),
            &initrd,
            Some(&disk.path),
            &format!("{config}\ncorium.halt=state"),
            "",
            &[(&path, sector)],
        );
        halted.expect("message=\"shutdown requested\" phase=\"state\"");
        assert!(!halted.output.contains(READY));
        timings(&halted.output, PHASES.len());
        h.outcome(
            &format!("state-{sector}-halt"),
            "PASS",
            "state prepared before init",
        );
        for (attempt, expected) in [
            ("first", "CORIUM-STATE-NEW"),
            ("retained", "CORIUM-STATE-RETAINED"),
            ("third", "CORIUM-STATE-RETAINED"),
        ] {
            let name = format!("state-{sector}-{attempt}");
            let boot = h.boot_with_storage(
                &name,
                &initrd,
                Some(&disk.path),
                &config,
                "",
                &[(&path, sector)],
            );
            boot.expect(READY);
            assert!(boot.output.contains(expected), "{}", boot.output);
            h.outcome(&name, "PASS", expected);
        }
    }
    // New declarations hide instance data; removing them exposes it unchanged.
    for version in ["B", "C"] {
        for path in [
            "var/lib/image/value",
            "var/lib/image-version",
            "etc/image-version",
        ] {
            fs::write(root.join(path), format!("{version}\n")).unwrap();
        }
        fs::write(root.join("var/local-metadata"), "image\n").unwrap();
        fs::write(
            root.join("var/.bind"),
            if version == "B" {
                "lib/image\nlib/image-version\nlocal-metadata\n"
            } else {
                "lib/image\nlib/image-version\n"
            },
        )
        .unwrap();
        let name = format!("state-replacement-{version}");
        let replacement = f::base(h, &root, &name, false);
        let digest = support::hash(&replacement.path);
        let boot = h.boot_with_storage(
            &name,
            &initrd,
            Some(&replacement.path),
            &format!("corium.root={}", replacement.root),
            "",
            &[(&h.work.join("state-512.raw"), 512)],
        );
        boot.expect(READY);
        assert!(
            boot.output.contains("CORIUM-STATE-RETAINED"),
            "{}",
            boot.output
        );
        assert_eq!(digest, support::hash(&replacement.path));
        h.outcome(
            &name,
            "PASS",
            "updated image binds and retained tmpfiles state",
        );
    }
    assert_eq!(original, support::hash(&disk.path));
}

#[test]
#[ignore = "requires QEMU/KVM, Linux 7+ kernel/modules and image tools"]
fn shutdown_handoff() {
    let h = harness();
    let _suite = h.suite("shutdown-handoff");
    let root = f::root(h, "shutdown-root");
    f::executable("/usr/lib/systemd/systemd-shutdown", &root);
    let service = root.join("etc/systemd/system/boot-complete.service");
    let text = std::fs::read_to_string(&service).unwrap();
    std::fs::write(
        &service,
        text.replace("poweroff-immediate", "poweroff-force"),
    )
    .unwrap();
    let disk = f::base(h, &root, "shutdown", false);
    let initrd = f::initramfs(h, "shutdown", false, false, "none");
    let boot = h.boot(
        "shutdown",
        &initrd,
        Some(&disk.path),
        &format!("corium.root={}", disk.root),
    );
    boot.expect(READY);
    assert!(
        !boot.output.contains("configure shutdown logging"),
        "{}",
        boot.output
    );
    assert!(
        !boot.output.contains("old root unmounted"),
        "{}",
        boot.output
    );
    assert!(
        !boot.output.contains("shutdown incomplete"),
        "{}",
        boot.output
    );
    h.outcome("shutdown", "PASS", "normal old-root unmount without dracut");

    std::fs::write(&service, text.replace("poweroff-immediate", "reboot-force")).unwrap();
    let disk = f::base(h, &root, "shutdown-reboot", false);
    let boot = h.boot(
        "shutdown-reboot",
        &initrd,
        Some(&disk.path),
        &format!("corium.root={}\ncorium.log=info", disk.root),
    );
    assert!(boot.success, "{}", boot.output);
    let event = boot.shutdown.as_ref().expect("missing shutdown event");
    assert_eq!(event["guest"], true);
    assert_eq!(event["reason"], "guest-reset");
    assert!(
        boot.output.contains("old root unmounted"),
        "{}",
        boot.output
    );
    assert!(
        !boot.output.contains("shutdown incomplete"),
        "{}",
        boot.output
    );
    h.outcome("shutdown-reboot", "PASS", "requested reboot preserved");
}
