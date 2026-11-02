// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use super::{Disk, record::Record};
use std::{fmt, io};
use uuid::Uuid;

/// Occupied partition metadata needed for discovery and diagnostics.
#[derive(Debug)]
pub(super) struct Entry {
    pub(super) number: u32,
    pub(super) kind: Uuid,
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) eligible: bool,
}

impl Entry {
    pub(super) fn new(number: u32, record: &Record) -> io::Result<Self> {
        let entry = Self {
            number,
            kind: Uuid::from_bytes_le(record.kind),
            start: record.start.get(),
            end: record.end.get(),
            eligible: record.attributes.get() & super::NO_AUTO == 0,
        };
        if entry.start > entry.end {
            return Err(Disk::invalid(format!("{entry} has reversed bounds")));
        }
        Ok(entry)
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "GPT partition {} (type {}, LBAs {}..={} in 4096-byte logical blocks)",
            self.number, self.kind, self.start, self.end
        )
    }
}
