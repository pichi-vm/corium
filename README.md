# Corium

Corium is a minimal Linux initramfs PID 1. Its sole handoff resource is a
mounted, read-only Carapace root filesystem. It executes `/sbin/init`; the real
init (systemd) owns all subsequent operating-system setup.

## Image-builder contract

- Supply Linux **7.0 or newer**, with device-mapper, devtmpfs, procfs, sysfs,
  bootconfig support (`CONFIG_BOOT_CONFIG=y`), and the drivers needed to
  discover and mount the root. Drivers may be built in or supplied as modules.
- Install the static Corium binary as `/init` in an initramfs.
- Put the required modules and their dependencies under
  `/usr/lib/modules/<running-kernel-release>/`, preserving subdirectories.
  Corium loads regular `.ko`, `.ko.xz`, `.ko.gz`, and `.ko.zst` files.
  Compressed files require the matching kernel decompressor. No module index,
  userspace decompressor, udev, shell, or device nodes are needed.
- Activate bootconfig with `bootconfig` on the kernel command line and supply
  `corium.root` in the bootconfig document. The value is the Carapace tip
  hash: nonempty, even-length hexadecimal, in either case, with no `0x`
  prefix. Carapace validates the digest length against the chain.
- Expose the Carapace's GPT partitions to the guest. The assembled Carapace
  must contain a filesystem directly, without an inner partition table.
- Include an executable `/sbin/init`, its interpreter/libraries, and the
  directories and files the real init requires in that filesystem. Corium
  does not create runtime mountpoints inside the read-only root.

## Boot behavior

1. Mount devtmpfs and reopen the console. Require Linux 7.0+ before mounting
   temporary procfs and sysfs.
2. Read and validate Corium's settings from `/proc/bootconfig`.
3. Recursively load the running kernel's module set. Already-loaded modules
   count as success. Repeat passes for dependencies; a pass that loads no
   additional module is fatal. An absent or empty module directory is valid.
4. Discover the root chain, allowing up to five seconds for missing partitions.
   Validation and other I/O failures are fatal immediately. Assemble it using
   Carapace's file-based API.
5. Read `/proc/filesystems`. Try the listed non-`nodev` drivers in order until
   a read-only mount succeeds. There is no hardcoded filesystem list.
6. Make mount propagation recursively private, then detach the temporary API
   mounts. Use `pivot_root(".", ".")` and detach the
   old initramfs, relying on Linux 7.0+'s rootfs semantics. There is no recursive
   deletion, writable old-root directory, or legacy-kernel fallback.
7. Execute `/sbin/init`. Console descriptors close on successful exec, but
   remain available for a fatal diagnostic if the handoff fails.

On fatal errors, Corium logs at error level, syncs, and attempts to power off.
It never returns from PID 1. Invoking it as an ordinary process fails without
performing init duties. Kernel and real-init output are outside Corium's
logging policy. Failures before the console is established may have no
Corium diagnostic if the kernel supplied no usable stderr.

Corium does not create `/run`, transfer API mounts to the new root, enforce
a JSON boot contract, configure networking or secondary volumes, provide a
writable root layer, or run as a post-boot service. It has no systemd generator,
init-path override, directory listing, or success-marker mode.

The pinned Carapace API currently wraps missing-partition errors as
`InvalidData`. The discovery adapter recognizes that specific diagnostic;
other validation errors are not retried. Recheck this adapter when updating
Carapace.

## Boot configuration

Corium consumes the top-level `corium` namespace from `/proc/bootconfig`:

```text
corium {
    root = "<Carapace tip hash in hexadecimal>";
    log = error;
    # halt = assembled;
}
```

`corium.root` is required. All Corium settings take one scalar value.
Unknown keys within `corium` are fatal; other namespaces are ignored.
Linux parses the document and resolves grouping and assignment operators;
Corium reads the resulting flattened, quoted key/value records.

Two optional settings control diagnostics:

- `corium.log = off|error|warn|info|debug|trace` sets the maximum verbosity.
  The default is `error`; successful boots are silent. `off` suppresses all
  Corium records, including fatal errors.
- `corium.halt = started|modules|discovered|assembled|mounted|pivoted`
  syncs and powers off after the selected phase completes successfully.
  Without it, Corium executes `/sbin/init`. It does not change the log level.

Diagnostic values are case-sensitive. Empty values and arrays are fatal.
Until the log setting can be read and validated, the filter is `error`.
A valid log setting applies before validation of the root and shutdown
settings, regardless of entry order. Invalid log settings leave the bootstrap
error filter in effect.

At `info` level, each completed phase produces a record:

```text
INFO corium: message="phase completed" phase="assembled" elapsed_us=28000 duration_us=8000
INFO corium: message="shutdown requested" phase="assembled"
```

The second record appears only at a requested shutdown boundary. Phase meanings:

| Phase | Work completed |
| --- | --- |
| `started` | Temporary mounts, console setup, and configuration validation |
| `modules` | All supplied modules loaded |
| `discovered` | Root chain discovered and validated |
| `assembled` | Carapace device activated |
| `mounted` | Root filesystem mounted read-only |
| `pivoted` | Root pivot and temporary/old mount detachment, before exec |

Both times are monotonic microseconds. `elapsed_us` measures time since entry
to the boot routine; `duration_us` measures this phase, including retries.
The preceding phase-completion record's write is excluded from the next
phase's duration, but included in total elapsed time.

`debug` adds module and filesystem selections and assembly details; `trace`
adds individual module, discovery, and mount attempts. `warn` is available
for noteworthy recoverable conditions; there are currently no warning records.
Normal dependency retries are trace events. Fatal errors use
`message="boot failed"` with a separate `error` field.

Records use Rust's `log` facade with structured key/value fields and a small
synchronous console logger. The format is `LEVEL target: message="..." key=value`.
Messages and string fields use JSON-style quoting and escaping; numbers and
booleans are unquoted. Each record occupies one line. Targets are `corium`
or its module paths (for example, `corium::modules`). Logging retains no extra
file descriptor and remains available after the pivot until successful exec.
Kernel and real-init output are unaffected by these settings.

## Build and validation

Rust 1.89 or newer is required; all build settings work on stable Rust.
Release builds optimize for size with LTO, stripped symbols, and abort-on-panic.
Run tests as an ordinary Linux user.

```sh
cargo build --locked --release --target x86_64-unknown-linux-musl
cargo test --locked
cargo fmt --all -- --check
RUSTFLAGS='-D warnings' cargo clippy --locked --all-targets --all-features
cargo deny check
```

Use `aarch64-unknown-linux-musl` for an ARM64 guest.

The rootless x86-64 VM tests are ignored Rust integration tests. They require
read/write access to `/dev/kvm`, a readable Linux 7.0+ kernel and matching
installed modules/configuration, stable Rust with the x86-64 musl target,
QEMU, host systemd, `cpio`, `sfdisk`, `mkfs.ext4`, `debugfs`, `e2fsck`,
`modprobe`, `veritysetup`, `ldd`, `ldconfig`, `sha256sum`, `df`, and `lsblk`.
Installed compressed modules also require the corresponding `xz`, `gzip`, or
`zstd` tool. No root, host mounts, loop devices, or host device-mapper changes
are needed. Required prerequisites fail explicitly; optional decompressor and
squashfs cases report `OMIT` with a reason, separately from Cargo's ignored
count. The suite requires at least 8 GiB free artifact storage per invocation.

```sh
cargo +stable test --locked --test boot -- --ignored --test-threads=1
cargo +stable test --locked --test benchmark -- --ignored --nocapture --test-threads=1
# Optional matching installed kernel override:
CORIUM_TEST_KERNEL=/path/to/vmlinuz CORIUM_TEST_KERNEL_RELEASE=<release> \
  cargo +stable test --locked --test boot -- --ignored --test-threads=1
```

The guest binary is built once per invocation using stable Rust's musl release
profile, then copied into the artifact directory. Host test code stays in the
ordinary test profile. Run VM tests and benchmarks sequentially on an otherwise
idle host; do not run separate invocations concurrently when comparing timings.

The shared large fixture has 50 scutes on one read-only virtio GPT disk with
100 partitions. A bootable ext4 base plus 44 small 4 MiB increments and five
160 MiB increments produces roughly 1 GiB of changed-block payload. Offline
filesystem edits, a Rust snapshot-store writer, and `veritysetup` produce the
chain without mounting it. Systemd runs `sha256sum` to check a surviving
sentinel from every layer and the final overwritten value before printing its
success marker and powering off. The bulk payload is not read in full during
validation. This is a synthetic demanding workload, not a full Fedora image.

Small fixtures exercise bootconfig validation, module failure/compression,
discovery timeout, corrupt metadata/data, missing intermediate scutes, failed
exec, halt boundaries, log filtering, and filesystem selection. Every expected
shutdown requires QMP's explicit guest-shutdown event. A deliberate panic
negative control ensures QEMU exit alone is rejected.

The informational benchmark uses the same large recipe and validation service:
two warm-up boots and ten measured boots, each in a fresh VM with
`corium.log = info` in bootconfig.
The primary metric is `pivoted.elapsed_us`, from entry to Corium's boot routine
through the final root switch. It excludes startup in `main`, the final log
write, and `exec`; successful systemd validation independently proves handoff.
It includes earlier logging overhead. Phase durations explain the total but
must not be summed to reconstruct it, because they exclude completion-record
writes. Fixture generation, QEMU/kernel startup, and systemd/shutdown are not
part of the metric. Host caches are warmed by construction and previous boots;
these are not cold-storage measurements. No host cache flushing is performed.
Report median/minimum/maximum and raw samples, with no timing pass/fail limit.

Artifacts remain under `target/boot-tests-*`: GPT/initramfs images, console and
QMP logs, case summaries, fixture sizes/roots, input hashes, command lines,
compiler/kernel/tool/host metadata, and benchmark CSV/summary. A failed run
retains its directory. Each invocation builds fresh fixtures, so tool and host
systemd changes are recorded rather than hidden by cross-run caching. Remove
obsolete `target/boot-tests-*` directories when their evidence is no longer
needed; they can occupy several GiB each. See CONTRIBUTING.md for the required
pre-merge correctness gate and runner setup.

## Consumer migration

Image builders install Corium as `/init`, enable bootconfig, and supply
`corium.root` with the Carapace tip hash. Fedora's module-tree layout matches
this contract. Carapace manages the assembled device's identity.

Version 0.2 removes the former library items `CORIUM_ROOT_OK`, `CORIUM_PIVOT`,
`parse_root_carapace`, and `decode_root_carapace`. The library now exposes
`boot` and `init_logging` for the PID 1 binary. External boot tests must
provide a real init and verify its handoff, rather than expect Corium to list
files and power off successfully when init is absent.
