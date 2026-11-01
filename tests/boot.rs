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
    let disk = f::base(h, &root, "shared-root", false);
    let initrd = f::initramfs(h, "shared-root", false, false, "none");
    let boot = h.boot(
        "shared-root",
        &initrd,
        Some(&disk.path),
        &format!("corium.root={}", disk.root),
    );
    boot.expect(READY);
    assert!(boot.output.contains("CORIUM-SHARED-ROOT-OK"));
    h.outcome(
        "shared-root",
        "PASS",
        "mounted root remains a reusable origin",
    );
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
            "no discoverable root for the target architecture",
        ),
        ("ambiguous-root", "multiple discoverable partitions"),
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
    let output = run("systemd-handoff", &config, READY);
    assert!(!output.contains("corium:"));
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
        timings(&output, 6);
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
    timings(&output, 6);
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
        ("bad-module", false, true, "module loading made no progress"),
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
    for (compression, option, tool) in [
        ("xz", "XZ", "xz"),
        ("gz", "GZIP", "gzip"),
        ("zst", "ZSTD", "zstd"),
    ] {
        let name = format!("modules-{compression}");
        if !h.config.contains("CONFIG_MODULE_DECOMPRESS=y")
            || !h
                .config
                .contains(&format!("CONFIG_MODULE_COMPRESS_{option}=y"))
        {
            h.outcome(&name, "OMIT", "guest kernel lacks this decompressor");
            continue;
        }
        if !support::available(tool) {
            h.outcome(&name, "OMIT", "compression tool not installed");
            continue;
        }
        let archive = f::initramfs(h, &name, false, false, compression);
        h.boot(&name, &archive, Some(&disk.path), &config)
            .expect(READY);
        h.outcome(&name, "PASS", "");
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
