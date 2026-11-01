// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

/// A validated partition extent in 512-byte sectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Partition {
    /// Offset within the assembled carapace or standalone disk image.
    pub(crate) start: u64,
    /// Number of sectors belonging to the partition.
    pub(crate) sectors: u64,
}

impl TryFrom<&gpt::partition::Partition> for Partition {
    type Error = std::io::Error;

    fn try_from(entry: &gpt::partition::Partition) -> Result<Self, Self::Error> {
        let invalid = || {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "partition mapping extent overflows",
            )
        };
        let start = entry
            .first_lba
            .checked_mul(super::SECTOR_SIZE / 512)
            .ok_or_else(invalid)?;
        let sectors = entry
            .last_lba
            .checked_sub(entry.first_lba)
            .and_then(|length| length.checked_add(1))
            .and_then(|length| length.checked_mul(super::SECTOR_SIZE / 512))
            .ok_or_else(invalid)?;
        Ok(Self { start, sectors })
    }
}
