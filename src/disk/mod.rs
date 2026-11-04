// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Discover the root in a bounded GPT disk inside a larger Carapace device.
//!
//! GPT 1.0 uses 4096-byte logical blocks and 128-byte entries. Both metadata
//! copies are checked without repair or filesystem probing. Only compact
//! occupied-entry records survive each block read; only the root is mapped.

mod entry;
mod header;
mod mbr_entry;
mod partition;
mod record;

pub(crate) use partition::Partition;

use crc::{CRC_32_ISO_HDLC, Crc};
use entry::Entry;
use header::Header;
use mbr_entry::MbrEntry;
use record::Record;
use std::io::{self, Read, Seek, SeekFrom};
use uuid::Uuid;
use zerocopy::FromBytes as _;

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
        let end = primary.backup_lba.get();
        if primary.current_lba.get() != 1 || !(11..capacity / SECTOR_SIZE).contains(&end) {
            return Err(Self::invalid(format!(
                "primary GPT header declares LBAs {} and {end} outside backing capacity {capacity} bytes",
                primary.current_lba.get()
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
        let length = u64::from(primary.num_parts.get()) * 128;
        let crc = Crc::<u32>::new(&CRC_32_ISO_HDLC);
        let mut primary_crc = crc.digest();
        let mut backup_crc = crc.digest();
        let mut disagreement = None;
        let mut entries = Vec::new();
        for offset in (0..length).step_by(SECTOR_SIZE as usize) {
            let index = offset / SECTOR_SIZE;
            let block = Self::block(
                reader,
                primary.part_start.get() + index,
                "primary GPT table",
            )?;
            let other = Self::block(reader, backup.part_start.get() + index, "backup GPT table")?;
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
            // The fixed wire layout borrows the current block. No names or
            // complete partition objects are decoded or retained.
            for (slot, bytes) in block[..used].as_chunks::<128>().0.iter().enumerate() {
                let raw = Record::ref_from_bytes(bytes).expect("128-byte, alignment-one record");
                if raw.is_used() {
                    let entry = Entry::new(offset as u32 / 128 + slot as u32 + 1, raw)?;
                    if entry.start < primary.first_usable.get()
                        || entry.end > primary.last_usable.get()
                    {
                        return Err(Self::invalid(format!(
                            "{entry} is outside usable GPT LBAs {}..={} (4096-byte logical blocks)",
                            primary.first_usable.get(),
                            primary.last_usable.get()
                        )));
                    }
                    entries.push(entry);
                }
            }
        }
        for (copy, calculated, expected) in [
            ("primary", primary_crc.finalize(), primary.crc32_parts.get()),
            ("backup", backup_crc.finalize(), backup.crc32_parts.get()),
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
        Header::read(&bytes, &context)
    }

    fn validate_headers(primary: &Header, backup: &Header) -> io::Result<()> {
        if backup.current_lba.get() != primary.backup_lba.get()
            || backup.backup_lba.get() != primary.current_lba.get()
            || backup.disk_guid != primary.disk_guid
            || backup.first_usable.get() != primary.first_usable.get()
            || backup.last_usable.get() != primary.last_usable.get()
            || backup.num_parts.get() != primary.num_parts.get()
            || backup.crc32_parts.get() != primary.crc32_parts.get()
        {
            return Err(Self::invalid(format!(
                "primary and backup GPT headers disagree: primary {primary:?}; backup {backup:?}"
            )));
        }
        let table_blocks = (u64::from(primary.num_parts.get()) * 128).div_ceil(SECTOR_SIZE);
        let start = primary.part_start.get();
        let first = primary.first_usable.get();
        let last = primary.last_usable.get();
        let backup_start = backup.part_start.get();
        let end = backup.current_lba.get();
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
        if bytes[510..512] != [0x55, 0xaa] {
            return Err(Self::invalid(
                "protective MBR at LBA 0 has an invalid signature",
            ));
        }
        let mut protective = 0;
        for (index, raw) in bytes[446..510].as_chunks::<16>().0.iter().enumerate() {
            let entry = MbrEntry::ref_from_bytes(raw).expect("16-byte, alignment-one MBR entry");
            if entry.protects(sectors) {
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
