// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

#![cfg(target_os = "linux")]
mod support;
use std::fmt::Write as _;
use std::fs::File;
use std::io::Write;
use support::qemu::{PHASES, READY, timings};
use support::{fixtures as f, harness};

#[test]
#[ignore = "requires QEMU/KVM and large fixture; informational repeated-boot benchmark"]
fn large_image_boot() {
    let h = harness();
    let _suite = h.suite("benchmark");
    let initrd = f::initramfs(h, "benchmark", false, false, "none");
    let (disk, _) = f::layered(h, "large", 50);
    let mut csv = File::create(h.work.join("timings.csv")).unwrap();
    writeln!(csv, "sample,warmup,phase,elapsed_us,duration_us").unwrap();
    let mut samples = Vec::new();
    for sample in 0..12 {
        let name = format!("benchmark-{sample:02}");
        let boot = h.boot(
            &name,
            &initrd,
            Some(&disk.path),
            &format!("corium.root={}; corium.log=info", disk.root),
        );
        boot.expect(READY);
        let rows = timings(&boot.output, 6);
        for row in &rows {
            writeln!(
                csv,
                "{sample},{},{},{},{}",
                sample < 2,
                row.phase,
                row.elapsed,
                row.duration
            )
            .unwrap();
        }
        csv.flush().unwrap();
        h.outcome(
            &name,
            "PASS",
            &format!("total_us={} warmup={}", rows[5].elapsed, sample < 2),
        );
        if sample >= 2 {
            samples.push(rows);
        }
    }
    let mut report = String::from(
        "Corium through pivoted; microseconds; 10 samples, 2 warmups\nFresh guest caches; warmed host cache; info logging enabled\nExcludes main startup, final log write and exec syscall\nmetric,median_us,min_us,max_us\n",
    );
    for index in 0..7 {
        let mut values: Vec<_> = samples
            .iter()
            .map(|rows| {
                if index == 6 {
                    rows[5].elapsed
                } else {
                    rows[index].duration
                }
            })
            .collect();
        values.sort_unstable();
        let median = format!(
            "{}{}",
            values[4].midpoint(values[5]),
            if (values[4] % 2) == (values[5] % 2) {
                ".0"
            } else {
                ".5"
            }
        );
        writeln!(
            report,
            "{},{median},{},{}",
            if index == 6 { "total" } else { PHASES[index] },
            values[0],
            values[9]
        )
        .unwrap();
    }
    std::fs::write(h.work.join("benchmark-summary.txt"), &report).unwrap();
    println!("{report}");
}
