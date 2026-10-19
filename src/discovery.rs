// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use std::io;
use std::time::{Duration, Instant};

use carapace::{Chain, FromGpt};

/// Discover the root chain, retrying missing partitions for up to five seconds.
pub(crate) fn discover(root: &[u8]) -> io::Result<Chain> {
    retry(|| Chain::from_gpt(root), Duration::from_secs(5))
}

fn retry<T>(mut discover: impl FnMut() -> io::Result<T>, timeout: Duration) -> io::Result<T> {
    let deadline = Instant::now() + timeout;
    let mut attempt = 0;
    loop {
        attempt += 1;
        log::trace!(attempt; "discovering root chain");
        match discover() {
            Ok(chain) => return Ok(chain),
            Err(error) if missing_partition(&error) => {
                log::trace!(attempt, error:% = error; "root partitions unavailable");
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("waiting for root partitions: {error}"),
                    ));
                }
                std::thread::sleep(remaining.min(Duration::from_millis(5)));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Carapace 99caf79 wraps missing partitions as InvalidData and discards the
/// underlying NotFound error. Match only that pinned API's discovery diagnostic;
/// every other InvalidData error (including verity validation) is fatal.
fn missing_partition(error: &io::Error) -> bool {
    if error.kind() != io::ErrorKind::InvalidData {
        return false;
    }
    let message = error.to_string();
    let Some(rest) = message.strip_prefix("chain walk failed at depth ") else {
        return false;
    };
    let Some((depth, reason)) = rest.split_once(": ") else {
        return false;
    };
    depth.parse::<usize>().is_ok()
        && (reason.starts_with("data partition not found: ")
            || reason.starts_with("hash partition not found: "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "chain walk failed at depth 1: hash partition not found: partition not found: PARTUUID test",
        )
    }

    #[test]
    fn missing_partitions_wait_for_the_retry_interval() {
        let timeout = Duration::from_millis(30);
        let started = Instant::now();
        let mut calls = 0;
        let error = retry::<()>(
            || {
                calls += 1;
                Err(missing())
            },
            timeout,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= timeout);
        assert!(calls >= 2);
    }

    #[test]
    fn retries_missing_partitions_then_returns_chain() {
        let mut calls = 0;
        let result = retry(
            || {
                calls += 1;
                if calls == 1 { Err(missing()) } else { Ok(42) }
            },
            Duration::from_secs(1),
        );
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls, 2);
    }

    #[test]
    fn missing_partition_deadline_is_fatal() {
        let error = retry::<()>(|| Err(missing()), Duration::ZERO).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn validation_and_io_failures_are_not_retried() {
        for kind in [
            io::ErrorKind::InvalidData,
            io::ErrorKind::InvalidInput,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::NotFound,
        ] {
            let mut calls = 0;
            let error = retry::<()>(
                || {
                    calls += 1;
                    Err(io::Error::new(kind, "bad superblock"))
                },
                Duration::from_secs(5),
            )
            .unwrap_err();
            assert_eq!(error.kind(), kind);
            assert_eq!(calls, 1);
        }
        assert!(!missing_partition(&io::Error::other(missing().to_string())));
    }
}
