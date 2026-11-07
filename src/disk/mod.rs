// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Discover the root in a bounded GPT disk inside a larger Carapace device.
//!
//! GPT 1.0 uses 4096-byte logical blocks and 128-byte entries. Both metadata
//! copies are checked without repair or filesystem probing. Tables are bounded
//! to 1 MiB, decoded by gpt, and only the selected root is mapped.

mod entry;
mod partition;

pub(crate) use partition::Partition;

use crc::{CRC_32_ISO_HDLC, Crc};
use entry::Entry;
use gpt::{disk::LogicalBlockSize, header::Header};
use std::io::{self, Read, Seek, SeekFrom};
use uuid::Uuid;

const SECTOR_SIZE: u64 = 4096;
const NO_AUTO: u64 = 1 << 63;
const MAX_ENTRIES: u32 = 8192;

#[cfg(target_arch = "x86_64")]
pub(crate) const ROOT_PARTITION_TYPE: Uuid = uuid::uuid!("4f68bce3-e8cd-4db1-96e7-fbcaf984b709");
#[cfg(target_arch = "aarch64")]
pub(crate) const ROOT_PARTITION_TYPE: Uuid = uuid::uuid!("b921b045-1df0-41c3-af44-4c6f280d3fae");
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("Corium requires an x86-64 or AArch64 discoverable root partition type");

/// A root selected after validating both copies of its disk's GPT metadata.
#[derive(Debug)]
pub(crate) struct Disk {
    pub(crate) root: Partition,
}

impl Disk {
    /// Absence of the GPT signature denotes a naked filesystem. Read failures,
    /// including dm-verity failures, and invalid GPT metadata remain errors.
    pub(crate) fn probe(
        reader: &mut (impl Read + Seek),
        capacity: u64,
        root_type: Uuid,
    ) -> io::Result<Option<Self>> {
        let bytes = Self::block(reader, 1, "root layout")?;
        if &bytes[..8] != b"EFI PART" {
            return Ok(None);
        }
        Self::read(reader, capacity, root_type).map(Some)
    }

    /// `capacity` bounds the backing device, not the smaller logical GPT disk.
    pub(crate) fn read(
        reader: &mut (impl Read + Seek),
        capacity: u64,
        root_type: Uuid,
    ) -> io::Result<Self> {
        if capacity > carapace::ZERO_SIZE_BYTES
            || !capacity.is_multiple_of(SECTOR_SIZE)
            || capacity < 12 * SECTOR_SIZE
        {
            return Err(Self::invalid(format!(
                "invalid GPT backing capacity: {capacity} bytes"
            )));
        }
        let primary = Self::header(reader, 1, "primary")?;
        let end = primary.backup_lba;
        if primary.current_lba != 1 || !(11..capacity / SECTOR_SIZE).contains(&end) {
            return Err(Self::invalid(format!(
                "primary GPT header declares LBAs {} and {end} outside backing capacity {capacity} bytes",
                primary.current_lba
            )));
        }
        let backup = Self::header(reader, end, "backup")?;
        Self::validate_headers(&primary, &backup)?;
        Self::protective_mbr(reader, end + 1)?;
        let entries = Self::entries(reader, &primary, &backup)?;
        let mut roots = entries
            .iter()
            .filter(|entry| entry.kind == root_type && entry.eligible);
        let root = roots.next().ok_or_else(|| {
            Self::invalid(format!(
                "no discoverable root for partition type {root_type} (no-auto roots excluded)"
            ))
        })?;
        if let Some(other) = roots.next() {
            return Err(Self::invalid(format!(
                "multiple discoverable root partitions: {root} and {other}"
            )));
        }
        Ok(Self {
            root: Partition::try_from(root)?,
        })
    }

    fn entries(
        reader: &mut (impl Read + Seek),
        primary: &Header,
        backup: &Header,
    ) -> io::Result<Vec<Entry>> {
        // Header validation bounds the table to 1 MiB before any iteration.
        let length = u64::from(primary.num_parts) * 128;
        let crc = Crc::<u32>::new(&CRC_32_ISO_HDLC);
        let mut primary_crc = crc.digest();
        let mut backup_crc = crc.digest();
        let mut disagreement = None;
        let mut table = Vec::with_capacity(length as usize);
        for offset in (0..length).step_by(SECTOR_SIZE as usize) {
            let index = offset / SECTOR_SIZE;
            let block = Self::block(reader, primary.part_start + index, "primary GPT table")?;
            let other = Self::block(reader, backup.part_start + index, "backup GPT table")?;
            let used = (length - offset).min(SECTOR_SIZE) as usize;
            primary_crc.update(&block[..used]);
            backup_crc.update(&other[..used]);
            if disagreement.is_none() {
                disagreement = block[..used]
                    .iter()
                    .zip(&other[..used])
                    .position(|(left, right)| left != right)
                    .map(|byte| (offset + byte as u64) / 128 + 1);
            }
            table.extend_from_slice(&block[..used]);
        }
        for (copy, calculated, expected) in [
            ("primary", primary_crc.finalize(), primary.crc32_parts),
            ("backup", backup_crc.finalize(), backup.crc32_parts),
        ] {
            if calculated != expected {
                return Err(Self::invalid(format!(
                    "{copy} GPT partition-array checksum mismatch: expected {expected:#010x}, calculated {calculated:#010x}"
                )));
            }
        }
        if let Some(number) = disagreement {
            return Err(Self::invalid(format!(
                "primary and backup GPT partition tables disagree at partition {number}"
            )));
        }
        // The crate decodes entries and checks its table CRC. Header policy above
        // bounds this buffer to 1 MiB before any disk-controlled allocation.
        let mut header = primary.clone();
        header.part_start = 0;
        let partitions = gpt::partition::file_read_partitions(
            &mut std::io::Cursor::new(table),
            &header,
            LogicalBlockSize::Lb4096,
        )?;
        let mut entries = Vec::new();
        for (number, partition) in partitions {
            // GPT marks unused slots by type, regardless of other slot bytes.
            if partition.part_type_guid.guid.is_nil() {
                continue;
            }
            let entry = Entry::new(number, &partition)?;
            if entry.start < primary.first_usable || entry.end > primary.last_usable {
                return Err(Self::invalid(format!(
                    "{entry} is outside usable GPT LBAs {}..={} (4096-byte logical blocks)",
                    primary.first_usable, primary.last_usable,
                )));
            }
            entries.push(entry);
        }
        entries.sort_unstable_by_key(|entry| entry.start);
        for pair in entries.windows(2) {
            if pair[0].end >= pair[1].start {
                return Err(Self::invalid(format!("{} overlaps {}", pair[0], pair[1])));
            }
        }
        Ok(entries)
    }

    fn block(reader: &mut (impl Read + Seek), lba: u64, context: &str) -> io::Result<[u8; 4096]> {
        let offset = lba.checked_mul(SECTOR_SIZE).ok_or_else(|| {
            Self::invalid(format!("{context} LBA {lba} overflows its byte offset"))
        })?;
        let mut bytes = [0; 4096];
        reader
            .seek(SeekFrom::Start(offset))
            .and_then(|_| reader.read_exact(&mut bytes))
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("read {context} at GPT LBA {lba} (byte offset {offset}): {error}"),
                )
            })?;
        Ok(bytes)
    }

    fn header(reader: &mut (impl Read + Seek), lba: u64, copy: &str) -> io::Result<Header> {
        let context = format!("{copy} GPT header at LBA {lba}");
        let bytes = Self::block(reader, lba, &context)?;
        // gpt's public header reader reads LBA 1. Decode the already-read block
        // in memory so physical reads retain their bounded contextual errors.
        let mut image = [0; 8192];
        image[4096..].copy_from_slice(&bytes);
        let header = gpt::header::read_header_from_arbitrary_device(
            &mut std::io::Cursor::new(image),
            LogicalBlockSize::Lb4096,
        )
        .map_err(|error| {
            use gpt::header::HeaderError;
            match error {
                HeaderError::InvalidGptSignature => {
                    Self::invalid(format!("{context}: invalid GPT signature"))
                }
                HeaderError::InvalidCRC32Checksum => {
                    // Decode only the diagnostic checksum after the crate rejects it.
                    let expected = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
                    image[4096 + 16..4096 + 20].fill(0);
                    let calculated = Crc::<u32>::new(&CRC_32_ISO_HDLC)
                        .checksum(&image[4096..4096 + 92]);
                    Self::invalid(format!(
                        "{context} checksum mismatch: expected {expected:#010x}, calculated {calculated:#010x}"
                    ))
                }
                error => Self::invalid(format!("{context}: {error}")),
            }
        })?;
        if header.revision != (1, 0)
            || header.header_size_le != 92
            || header.reserved != 0
            || header.part_size != 128
            || !(128..=MAX_ENTRIES).contains(&header.num_parts)
            || header.disk_guid.is_nil()
            || bytes[92..].iter().any(|byte| *byte != 0)
        {
            return Err(Self::invalid(format!(
                "{context}: unsupported header or entry geometry: {header:?}; requires GPT 1.0, a 92-byte header, 128-byte entries, 128..8192 slots, a nonzero disk UUID and zero reserved bytes"
            )));
        }
        Ok(header)
    }

    fn validate_headers(primary: &Header, backup: &Header) -> io::Result<()> {
        if backup.current_lba != primary.backup_lba
            || backup.backup_lba != primary.current_lba
            || backup.disk_guid != primary.disk_guid
            || backup.first_usable != primary.first_usable
            || backup.last_usable != primary.last_usable
            || backup.num_parts != primary.num_parts
            || backup.crc32_parts != primary.crc32_parts
        {
            return Err(Self::invalid(format!(
                "primary and backup GPT headers disagree: primary {primary:?}; backup {backup:?}"
            )));
        }
        let table_blocks = (u64::from(primary.num_parts) * 128).div_ceil(SECTOR_SIZE);
        let start = primary.part_start;
        let first = primary.first_usable;
        let last = primary.last_usable;
        let backup_start = backup.part_start;
        let end = backup.current_lba;
        if start < 2
            || start > end
            || start + table_blocks > first
            || first > last
            || last >= backup_start
            || backup_start > end
            || backup_start + table_blocks > end
        {
            return Err(Self::invalid(format!(
                "GPT metadata overlaps usable sectors or exceeds the disk: primary table LBA {start}, backup table LBA {backup_start}, {table_blocks} blocks each; usable LBAs {first}..={last}; backup header LBA {end} (4096-byte logical blocks)"
            )));
        }
        Ok(())
    }

    fn protective_mbr(reader: &mut (impl Read + Seek), sectors: u64) -> io::Result<()> {
        let bytes = Self::block(reader, 0, "protective MBR")?;
        if bytes[512..].iter().any(|byte| *byte != 0) {
            return Err(Self::invalid(
                "protective MBR at LBA 0 has nonzero reserved bytes",
            ));
        }
        let mbr = gpt::mbr::ProtectiveMBR::from_bytes(&bytes, LogicalBlockSize::Lb4096)
            .map_err(|error| Self::invalid(format!("protective MBR at LBA 0: {error}")))?;
        let mut protective = 0;
        for (index, raw) in bytes[446..510].as_chunks::<16>().0.iter().enumerate() {
            let entry = mbr.partition(index).expect("four MBR partition slots");
            if entry.os_type == 0xee
                && entry.boot_indicator == 0
                && entry.lb_start == 1
                && u64::from(entry.lb_size) == (sectors - 1).min(u64::from(u32::MAX))
            {
                protective += 1;
            } else if *raw != [0; 16] {
                return Err(Self::invalid(format!(
                    "invalid or hybrid protective MBR entry {}: {entry:?}",
                    index + 1
                )));
            }
        }
        if protective != 1 {
            return Err(Self::invalid(format!(
                "GPT requires one protective MBR entry; found {protective}"
            )));
        }
        Ok(())
    }

    fn invalid(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, error)
    }
}

#[cfg(test)]
mod tests;
