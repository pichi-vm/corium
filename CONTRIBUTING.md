# Contributing

## Commit messages

Commits follow Conventional Commits and standard git formatting, enforced by
gitlint (see `.gitlint`) in CI.

- Subject: `type(scope): summary` — lowercase, imperative, no trailing period,
  at most 50 characters. Types: `feat`, `fix`, `docs`, `style`, `refactor`,
  `perf`, `test`, `build`, `ci`, `chore`, `revert`, `spec`.
- One blank line, then a body wrapped at 72 characters explaining what changed
  and why (not how — the diff shows how).
- Write each message to stand on its own: describe the change and its rationale
  for a reader with no knowledge of the project's history or how the work was
  produced. Do not reference internal process, prior iterations, or review
  back-and-forth. (gitlint cannot check this — it is a review responsibility.)

## Validation required before merge

Run as an ordinary user on Linux (the non-PID-1 test refuses root):

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
cargo +1.89.0 test --locked
cargo deny check
cargo +stable test --locked --test boot -- --ignored --test-threads=1
```

The `VM boot validation` workflow runs automatically on pull requests and
pushes, alongside hosted CI. Require its `boot` job to pass before merging;
the workflow uploads console logs and pass/optional-case omission summaries.
Require both ordinary CI and boot validation before merging.
The suite exercises the real Carapace missing-partition diagnostic, including
its string-matched retry adapter, so dependency updates must pass it too.

The workflow uses GitHub-hosted `ubuntu-26.04` runners and their Linux 7.0+
kernel, matching modules and configuration. Setup installs image-building
tools and stable Rust's x86_64 musl target, grants the runner user KVM access,
and makes the guest kernel readable. The test suite itself runs without root.
No self-hosted runner is needed. Manual dispatch remains available for reruns;
it is not required for normal PR validation.

Hosted CI tests stable and Rust 1.89 and builds the x86_64 and ARM64 musl
binaries. Non-Linux library builds are compatibility stubs; they do not run
the Linux tests. Cargo.lock is included for reproducible binary builds;
update and review it deliberately. Release builds use stable size optimization, LTO, symbol stripping, and
abort-on-panic. Handled boot errors still log and power off; unexpected panics
terminate PID 1. Nightly-only build features are not supported.

## Source headers

New Rust, Python, and shell files must carry the SPDX copyright and license
header used by existing sources. Hawkeye checks this policy via
licenserc.toml. Keep full license text in LICENSE.

The informational benchmark is explicit and separate from the correctness gate:

```sh
cargo +stable test --locked --test benchmark -- --ignored --nocapture --test-threads=1
```

Keep the host idle and retain the timing CSV, summary, and environment metadata.
See README for the exact timing boundary and cache policy.
