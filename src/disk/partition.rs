// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use super::{Disk, entry::Entry};

/// A validated partition extent in 512-byte device-mapper sectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Partition {
    pub(crate) start: u64,
    pub(crate) sectors: u64,
}

impl TryFrom<&Entry> for Partition {
    type Error = std::io::Error;

    fn try_from(entry: &Entry) -> Result<Self, Self::Error> {
        let invalid = || {
            Disk::invalid(format!(
                "{entry} cannot be converted to 512-byte device-mapper sectors: invalid or overflowing extent"
            ))
        };
        Ok(Self {
            start: entry
                .start
                .checked_mul(super::SECTOR_SIZE / 512)
                .ok_or_else(invalid)?,
            sectors: entry
                .end
                .checked_sub(entry.start)
                .and_then(|length| length.checked_add(1))
                .and_then(|length| length.checked_mul(super::SECTOR_SIZE / 512))
                .ok_or_else(invalid)?,
        })
    }
}
