// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Reuse valid state partitions; initialize GPT only on wholly blank disks.

use gpt::{disk::LogicalBlockSize, header::Header, mbr::ProtectiveMBR};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use uuid::Uuid;

pub(super) const STATE_TYPE: Uuid = uuid::uuid!("32a2f055-1a43-44b2-ab7a-483ff5f13481");
const ALIGNMENT: u64 = 1024 * 1024;
const TABLE_BYTES: usize = 128 * 128;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Return the state extent and whether this invocation created its GPT.
/// A failed or interrupted previous initialization is never retried by formatting.
pub(super) fn prepare(
    file: &mut File,
    capacity: u64,
    sector: LogicalBlockSize,
) -> io::Result<(gpt::partition::Partition, bool)> {
    let block = u64::from(sector);
    if capacity < 4 * ALIGNMENT || !capacity.is_multiple_of(block) {
        return Err(invalid(
            "state disk must be at least 4 MiB and sector-aligned",
        ));
    }
    file.seek(SeekFrom::Start(block))?;
    let mut signature = [0; 8];
    file.read_exact(&mut signature)?;
    if &signature == b"EFI PART" {
        return read(file, capacity, sector).map(|partition| (partition, false));
    }

    // Missing GPT is not permission to destroy data. Check the entire device,
    // including its backup metadata and any data outside the proposed partition.
    file.rewind()?;
    let mut remaining = capacity;
    let mut buffer = vec![0; ALIGNMENT as usize];
    while remaining != 0 {
        let length = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..length])?;
        if buffer[..length].iter().any(|byte| *byte != 0) {
            return Err(invalid(
                "state disk is not blank and has no valid primary GPT; refusing to format",
            ));
        }
        remaining -= length as u64;
    }

    // Persist a nonblank marker before any GPT/formatter writes. An interrupted
    // initialization must fail validation on the next boot, not format again.
    ProtectiveMBR::with_lb_size((capacity / block - 1).min(u64::from(u32::MAX)) as u32)
        .overwrite_lba0(file)
        .map_err(io::Error::other)?;
    file.sync_all()?;
    let mut disk = gpt::GptConfig::new()
        .writable(true)
        .logical_block_size(sector)
        .create_from_device(&mut *file, None)
        .map_err(io::Error::other)?;
    let first = disk.header().first_usable.div_ceil(ALIGNMENT / block) * (ALIGNMENT / block);
    let length = disk.header().last_usable - first + 1;
    disk.add_partition_at("pichi-state", 1, first, length, STATE_TYPE.into(), 0)
        .map_err(io::Error::other)?;
    disk.write().map_err(io::Error::other)?.sync_all()?;
    read(file, capacity, sector).map(|partition| (partition, true))
}

fn header(file: &mut File, lba: u64, sector: LogicalBlockSize) -> io::Result<Header> {
    let size = usize::from(sector);
    let mut bytes = vec![0; 2 * size];
    file.seek(SeekFrom::Start(lba * u64::from(sector)))?;
    file.read_exact(&mut bytes[size..])?;
    if bytes[size + 92..].iter().any(|byte| *byte != 0) {
        return Err(invalid("state GPT header has nonzero reserved bytes"));
    }
    let h = gpt::header::read_header_from_arbitrary_device(&mut io::Cursor::new(bytes), sector)
        .map_err(io::Error::other)?;
    // Bound all crate table allocations before passing it disk-controlled counts.
    if h.revision != (1, 0)
        || h.header_size_le != 92
        || h.reserved != 0
        || h.num_parts != 128
        || h.part_size != 128
        || h.disk_guid.is_nil()
    {
        return Err(invalid("unsupported state GPT geometry"));
    }
    Ok(h)
}

fn read(
    file: &mut File,
    capacity: u64,
    sector: LogicalBlockSize,
) -> io::Result<gpt::partition::Partition> {
    let block = u64::from(sector);
    if capacity < 4 * ALIGNMENT || !capacity.is_multiple_of(block) {
        return Err(invalid(
            "state disk must be at least 4 MiB and sector-aligned",
        ));
    }
    let end = capacity / block - 1;
    let table_blocks = TABLE_BYTES as u64 / block;
    let primary = header(file, 1, sector)?;
    let backup = header(file, end, sector)?;
    if primary.current_lba != 1
        || primary.backup_lba != end
        || backup.current_lba != end
        || backup.backup_lba != 1
        || primary.disk_guid != backup.disk_guid
        || primary.part_start != 2
        || backup.part_start != end - table_blocks
        || primary.first_usable != 2 + table_blocks
        || primary.last_usable != end - table_blocks - 1
        || backup.first_usable != primary.first_usable
        || backup.last_usable != primary.last_usable
        || primary.crc32_parts != backup.crc32_parts
    {
        return Err(invalid(
            "state GPT copies disagree or have unexpected bounds",
        ));
    }
    file.rewind()?;
    let mut mbr = vec![0; block as usize];
    file.read_exact(&mut mbr)?;
    let expected = ProtectiveMBR::with_lb_size(end.min(u64::from(u32::MAX)) as u32).to_bytes();
    if mbr[..512] != expected || mbr[512..].iter().any(|byte| *byte != 0) {
        return Err(invalid("state disk has an invalid protective MBR"));
    }
    let mut tables = vec![[0; TABLE_BYTES]; 2];
    for (table, h) in tables.iter_mut().zip([&primary, &backup]) {
        file.seek(SeekFrom::Start(h.part_start * block))?;
        file.read_exact(table)?;
    }
    if tables[0] != tables[1] {
        return Err(invalid("state GPT partition tables disagree"));
    }
    let entries = gpt::partition::file_read_partitions(file, &primary, sector)?;
    let partition = entries
        .get(&1)
        .ok_or_else(|| invalid("missing state partition"))?;
    if entries.len() != 1
        || partition.part_type_guid.guid != STATE_TYPE
        || partition.part_guid.is_nil()
        || partition.flags != 0
        || partition.first_lba < primary.first_usable
        || partition.last_lba > primary.last_usable
        || partition.first_lba > partition.last_lba
        || !(partition.first_lba * block).is_multiple_of(ALIGNMENT)
    {
        return Err(invalid(
            "state disk requires one aligned pichi-state partition",
        ));
    }
    Ok(partition.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(sector: LogicalBlockSize) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.set_len(8 * ALIGNMENT).unwrap();
        let block = u64::from(sector);
        ProtectiveMBR::with_lb_size((8 * ALIGNMENT / block - 1) as u32)
            .overwrite_lba0(&mut file)
            .unwrap();
        let mut disk = gpt::GptConfig::new()
            .writable(true)
            .logical_block_size(sector)
            .create_from_device(&mut file, None)
            .unwrap();
        let first = ALIGNMENT / block;
        let length = disk.header().last_usable - first + 1;
        disk.add_partition_at("pichi-state", 1, first, length, STATE_TYPE.into(), 0)
            .unwrap();
        disk.write().unwrap();
        file
    }

    fn bytes(file: &mut File) -> Vec<u8> {
        file.rewind().unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn reads_existing_state_without_writes() {
        for sector in [LogicalBlockSize::Lb512, LogicalBlockSize::Lb4096] {
            let mut file = fixture(sector);
            let before = bytes(&mut file);
            let partition = read(&mut file, 8 * ALIGNMENT, sector).unwrap();
            assert_eq!(partition.first_lba * u64::from(sector), ALIGNMENT);
            assert_eq!(bytes(&mut file), before);
        }
    }

    #[test]
    fn corrupt_metadata_is_not_repaired() {
        for sector in [LogicalBlockSize::Lb512, LogicalBlockSize::Lb4096] {
            let block = u64::from(sector);
            for offset in [
                0,
                block,
                2 * block,
                8 * ALIGNMENT - block,
                8 * ALIGNMENT - 2 * block,
            ] {
                let mut file = fixture(sector);
                file.seek(SeekFrom::Start(offset)).unwrap();
                file.write_all(&[0xa5]).unwrap();
                let before = bytes(&mut file);
                assert!(read(&mut file, 8 * ALIGNMENT, sector).is_err());
                assert_eq!(bytes(&mut file), before);
            }
        }
    }

    #[test]
    fn rejects_other_partition_types_and_blank_disks() {
        let sector = LogicalBlockSize::Lb512;
        let mut file = fixture(sector);
        let mut disk = gpt::GptConfig::new()
            .writable(true)
            .logical_block_size(sector)
            .open_from_device(&mut file)
            .unwrap();
        let mut entries = disk.partitions().clone();
        entries.get_mut(&1).unwrap().part_type_guid = gpt::partition_types::EFI;
        disk.update_partitions(entries).unwrap();
        disk.write().unwrap();
        let before = bytes(&mut file);
        assert!(read(&mut file, 8 * ALIGNMENT, sector).is_err());
        assert_eq!(bytes(&mut file), before);
        let mut blank = tempfile::tempfile().unwrap();
        blank.set_len(8 * ALIGNMENT).unwrap();
        assert!(read(&mut blank, 8 * ALIGNMENT, sector).is_err());
        assert!(bytes(&mut blank).iter().all(|byte| *byte == 0));
        for size in [0, 512, 4096, 4 * ALIGNMENT - 1] {
            assert!(read(&mut blank, size, sector).is_err());
        }
    }

    #[test]
    fn initializes_once_with_unique_ids_and_both_sector_sizes() {
        for sector in [LogicalBlockSize::Lb512, LogicalBlockSize::Lb4096] {
            let mut first = tempfile::tempfile().unwrap();
            first.set_len(8 * ALIGNMENT).unwrap();
            let (partition, created) = prepare(&mut first, 8 * ALIGNMENT, sector).unwrap();
            assert!(created);
            assert_eq!(partition.first_lba * u64::from(sector), ALIGNMENT);
            let mut before = Vec::new();
            first.rewind().unwrap();
            first.read_to_end(&mut before).unwrap();
            let (again, created) = prepare(&mut first, 8 * ALIGNMENT, sector).unwrap();
            assert!(!created);
            assert_eq!(partition, again);
            let mut after = Vec::new();
            first.rewind().unwrap();
            first.read_to_end(&mut after).unwrap();
            assert_eq!(before, after);
            let mut second = tempfile::tempfile().unwrap();
            second.set_len(8 * ALIGNMENT).unwrap();
            let (other, _) = prepare(&mut second, 8 * ALIGNMENT, sector).unwrap();
            assert_ne!(partition.part_guid, other.part_guid);
            assert_ne!(
                header(&mut first, 1, sector).unwrap().disk_guid,
                header(&mut second, 1, sector).unwrap().disk_guid
            );
        }
    }

    #[test]
    fn unknown_data_and_interrupted_initialization_are_unchanged() {
        for offset in [0, 1024 * 1024, 8 * ALIGNMENT - 1] {
            let mut file = tempfile::tempfile().unwrap();
            file.set_len(8 * ALIGNMENT).unwrap();
            file.seek(SeekFrom::Start(offset)).unwrap();
            file.write_all(&[0xa5]).unwrap();
            assert!(prepare(&mut file, 8 * ALIGNMENT, LogicalBlockSize::Lb512).is_err());
            file.rewind().unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes.iter().filter(|b| **b != 0).count(), 1);
            assert_eq!(bytes[offset as usize], 0xa5);
        }
    }
}
