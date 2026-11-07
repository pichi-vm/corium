// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use super::*;
use gpt::{
    disk::LogicalBlockSize, header::Header, partition::Partition as RawEntry, partition_types,
};
use std::{collections::BTreeMap, io::Cursor};
use uuid::uuid;

const GPT_BLOCK_SIZE: LogicalBlockSize = LogicalBlockSize::Lb4096;
const X64: Uuid = uuid!("4f68bce3-e8cd-4db1-96e7-fbcaf984b709");
const ARM64: Uuid = uuid!("b921b045-1df0-41c3-af44-4c6f280d3fae");
use std::io::Write;

const SIZE: usize = 8 * 1024 * 1024;

#[derive(Debug)]
struct Fixture {
    bytes: Cursor<Vec<u8>>,
}

impl Fixture {
    fn new(types: &[gpt::partition_types::Type]) -> Self {
        let mut bytes = Cursor::new(vec![0; SIZE]);
        gpt::mbr::ProtectiveMBR::with_lb_size((SIZE / 4096 - 1) as u32)
            .overwrite_lba0(&mut bytes)
            .unwrap();
        let mut disk = gpt::GptConfig::new()
            .writable(true)
            .logical_block_size(GPT_BLOCK_SIZE)
            .create_from_device(bytes, None)
            .unwrap();
        for kind in types {
            disk.add_partition("root", 1024 * 1024, kind.clone(), 0, Some(256))
                .unwrap();
        }
        Self {
            bytes: disk.write().unwrap(),
        }
    }

    fn read(&mut self) -> io::Result<Partition> {
        self.read_as(X64)
    }

    fn read_as(&mut self, kind: Uuid) -> io::Result<Partition> {
        Disk::read(&mut self.bytes, SIZE as u64, kind).map(|disk| disk.root)
    }

    fn header(&self, lba: u64) -> Header {
        let mut bytes = vec![0; 8192];
        let start = lba as usize * 4096;
        bytes[4096..].copy_from_slice(&self.bytes.get_ref()[start..start + 4096]);
        gpt::header::read_header_from_arbitrary_device(&mut Cursor::new(bytes), GPT_BLOCK_SIZE)
            .unwrap()
    }

    fn headers(&mut self, edit: impl FnOnce(&mut Header, &mut Header)) {
        let mut primary = self.header(1);
        let mut backup = self.header(primary.backup_lba);
        edit(&mut primary, &mut backup);
        primary
            .write_primary(&mut self.bytes, GPT_BLOCK_SIZE)
            .unwrap();
        backup
            .write_backup(&mut self.bytes, GPT_BLOCK_SIZE)
            .unwrap();
    }

    fn entries(&mut self, edit: impl FnOnce(&mut BTreeMap<u32, RawEntry>)) {
        let disk = gpt::GptConfig::new()
            .writable(true)
            .logical_block_size(GPT_BLOCK_SIZE)
            .open_from_device(&mut self.bytes)
            .unwrap();
        let primary = disk.primary_header().unwrap().clone();
        let backup = disk.backup_header().unwrap().clone();
        let mut entries = disk.partitions().clone();
        drop(disk);
        edit(&mut entries);
        for start in [primary.part_start, backup.part_start] {
            let offset = start as usize * 4096;
            self.bytes.get_mut()[offset..offset + primary.num_parts as usize * 128].fill(0);
        }
        for (index, entry) in entries {
            for start in [primary.part_start, backup.part_start] {
                entry
                    .write_to_device(
                        &mut self.bytes,
                        u64::from(index - 1),
                        start,
                        GPT_BLOCK_SIZE,
                        128,
                    )
                    .unwrap();
            }
        }
        self.headers(|_, _| {});
    }
}

#[test]
fn probe_distinguishes_naked_filesystems_from_gpt() {
    let mut bytes = Cursor::new(vec![0xa5; 8192]);
    assert!(
        Disk::probe(&mut bytes, carapace::ZERO_SIZE_BYTES, X64)
            .unwrap()
            .is_none()
    );
    let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
    assert_eq!(
        Disk::probe(&mut fixture.bytes, carapace::ZERO_SIZE_BYTES, X64)
            .unwrap()
            .unwrap()
            .root,
        Partition {
            start: 2048,
            sectors: 2048
        }
    );
}

#[test]
fn probe_preserves_gpt_validation_errors() {
    for offset in [4096 + 56, SIZE - 4096, 8192] {
        let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
        fixture.bytes.get_mut()[offset] ^= 1;
        assert_eq!(
            Disk::probe(&mut fixture.bytes, SIZE as u64, X64)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn probe_propagates_read_failures() {
    let mut spy = ReadSpy {
        bytes: Cursor::new(vec![0; 8192]),
        reads: Vec::new(),
        fail_at: Some(4096),
    };
    let error = Disk::probe(&mut spy, carapace::ZERO_SIZE_BYTES, X64).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(error.to_string().contains("injected device failure"));
    spy.fail_at = None;
    spy.bytes.get_mut().truncate(4096);
    assert_eq!(
        Disk::probe(&mut spy, carapace::ZERO_SIZE_BYTES, X64)
            .unwrap_err()
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn selects_native_root_beside_esp_and_foreign_root() {
    let mut fixture = Fixture::new(&[
        partition_types::LINUX_ROOT_X64,
        partition_types::EFI,
        partition_types::LINUX_ROOT_ARM_64,
    ]);
    assert_eq!(
        fixture.read().unwrap(),
        Partition {
            start: 2048,
            sectors: 2048
        }
    );
    assert_eq!(fixture.read_as(ARM64).unwrap().start, 6144);
    let message = Fixture::new(&[partition_types::LINUX_ROOT_ARM_64])
        .read()
        .unwrap_err()
        .to_string();
    assert!(message.contains(&X64.to_string()));
    assert!(message.contains("no discoverable root"));
    assert_eq!(
        ROOT_PARTITION_TYPE,
        if cfg!(target_arch = "x86_64") {
            X64
        } else {
            ARM64
        }
    );
}

#[test]
fn bounded_disk_can_live_inside_a_larger_carapace() {
    let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
    let disk = Disk::read(&mut fixture.bytes, carapace::ZERO_SIZE_BYTES, X64).unwrap();
    assert_eq!(disk.root.start, 2048);
    assert_eq!(fixture.bytes.get_ref().len(), SIZE);
    assert!(Disk::read(&mut fixture.bytes, SIZE as u64 - 4096, X64).is_err());
    assert!(Disk::read(&mut fixture.bytes, carapace::ZERO_SIZE_BYTES + 512, X64).is_err());
    assert!(Disk::read(&mut fixture.bytes, SIZE as u64 + 1, X64).is_err());
}

#[test]
fn rejects_ambiguous_roots_and_honors_no_auto() {
    let mut fixture = Fixture::new(&[
        partition_types::LINUX_ROOT_X64,
        partition_types::LINUX_ROOT_X64,
    ]);
    let message = fixture.read().unwrap_err().to_string();
    for expected in [
        "multiple discoverable root",
        "partition 1",
        "partition 2",
        &X64.to_string(),
    ] {
        assert!(message.contains(expected), "{message}");
    }
    fixture.entries(|entries| entries.get_mut(&1).unwrap().flags = NO_AUTO);
    assert_eq!(fixture.read().unwrap().start, 4096);
    fixture.entries(|entries| entries.get_mut(&2).unwrap().flags = NO_AUTO);
    assert!(fixture.read().unwrap_err().to_string().contains("no-auto"));
}

#[test]
fn checks_both_headers_and_tables_without_repair() {
    for offset in [4096 + 56, SIZE - 4096 + 56, 8192, SIZE - 5 * 4096] {
        let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
        fixture.bytes.get_mut()[offset] ^= 1;
        let before = fixture.bytes.get_ref().clone();
        let message = fixture.read().unwrap_err().to_string();
        if offset == 4096 + 56 || offset == SIZE - 4096 + 56 {
            let copy = if offset == 4096 + 56 {
                "primary"
            } else {
                "backup"
            };
            assert!(
                message.contains(&format!("{copy} GPT header at LBA")),
                "{message}"
            );
            assert!(
                message.contains("expected 0x") && message.contains("calculated 0x"),
                "{message}"
            );
        }
        assert_eq!(*fixture.bytes.get_ref(), before);
    }
    let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
    fixture.headers(|_, backup| {
        backup.disk_guid = "11111111-2222-3333-4444-555555555555".parse().unwrap();
    });
    assert!(
        fixture
            .read()
            .unwrap_err()
            .to_string()
            .contains("headers disagree")
    );
}

#[test]
fn rejects_unsupported_geometry_before_partition_decoding() {
    // These headers have correct CRCs; their entry geometry is still invalid.
    for (size, count) in [(64, 128), (256, 128), (128, 0), (128, MAX_ENTRIES + 1)] {
        let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
        // Only update the primary: the writer reads count*size bytes to compute
        // its checksum, so this fixture leaves ample readable zero-filled space.
        let mut primary = fixture.header(1);
        primary.part_size = size;
        primary.num_parts = count;
        primary
            .write_primary(&mut fixture.bytes, GPT_BLOCK_SIZE)
            .unwrap();
        assert!(fixture.read().unwrap_err().to_string().contains("geometry"));
    }
}

#[test]
fn rejects_invalid_headers_even_with_a_valid_checksum() {
    for lba in [1, SIZE / 4096 - 1] {
        for (offset, value, expected) in [
            (0, b"NOT GPT!".as_slice(), "signature"),
            (8, &[0, 0, 2, 0], "geometry"),
            (12, &[0, 0, 0, 0], "geometry"),
            (12, &[0xff; 4], "geometry"),
            (20, &[1, 0, 0, 0], "geometry"),
            (56, &[0; 16], "geometry"),
            (92, &[1], "geometry"),
        ] {
            let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
            assert!(fixture.read().is_ok());
            let bytes = &mut fixture.bytes.get_mut()[lba * 4096..(lba + 1) * 4096];
            bytes[offset..offset + value.len()].copy_from_slice(value);
            bytes[16..20].fill(0);
            let crc = Crc::<u32>::new(&CRC_32_ISO_HDLC).checksum(&bytes[..92]);
            bytes[16..20].copy_from_slice(&crc.to_le_bytes());
            let message = fixture.read().unwrap_err().to_string();
            let copy = if lba == 1 { "primary" } else { "backup" };
            assert!(message.contains(&format!("{copy} GPT header at LBA {lba}")));
            assert!(message.contains(expected), "{message}");
        }
    }
}

#[test]
fn accepts_one_protective_mbr_entry_in_any_slot() {
    for index in 0..4 {
        let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
        let bytes = fixture.bytes.get_mut();
        let entry: [u8; 16] = bytes[446..462].try_into().unwrap();
        bytes[446..510].fill(0);
        let start = 446 + index * 16;
        bytes[start..start + 16].copy_from_slice(&entry);
        assert!(fixture.read().is_ok());
        let other = 446 + (index + 1) % 4 * 16;
        fixture.bytes.get_mut()[other..other + 16].copy_from_slice(&entry);
        assert!(fixture.read().unwrap_err().to_string().contains("found 2"));
        fixture.bytes.get_mut()[446..510].fill(0);
        assert!(fixture.read().unwrap_err().to_string().contains("found 0"));
    }
}

#[test]
fn rejects_out_of_bounds_and_overlapping_partitions() {
    for mode in 0..3 {
        let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64, partition_types::EFI]);
        fixture.entries(|entries| {
            let first = entries[&1].clone();
            let entry = entries.get_mut(&2).unwrap();
            match mode {
                0 => entry.first_lba = first.last_lba,
                1 => entry.last_lba = SIZE as u64 / 4096,
                2 => entry.first_lba = entry.last_lba + 1,
                _ => unreachable!(),
            }
        });
        assert!(
            fixture.read().is_err(),
            "accepted malformed partition case {mode}"
        );
    }
}

#[test]
fn rejects_metadata_inside_partitions_and_unbounded_addresses() {
    for mode in 0..4 {
        let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
        let mut primary = fixture.header(1);
        match mode {
            0 => primary.first_usable = 2,
            1 => primary.last_usable = SIZE as u64 / 4096 - 1,
            2 => primary.backup_lba = u64::MAX,
            3 => primary.revision = (2, 0),
            _ => unreachable!(),
        }
        primary
            .write_primary(&mut fixture.bytes, GPT_BLOCK_SIZE)
            .unwrap();
        assert!(fixture.read().is_err());
    }
}

#[test]
fn rejects_truncated_and_nonprotective_disks() {
    for length in [0, 512, 1023, SIZE - 1] {
        let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
        fixture.bytes.get_mut().truncate(length);
        assert!(fixture.read().is_err());
    }
    for offset in [510, 446 + 4, 446 + 8, 446 + 12, 462 + 4, 512] {
        let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
        fixture.bytes.seek(SeekFrom::Start(offset)).unwrap();
        fixture.bytes.write_all(&[0x83]).unwrap();
        assert!(fixture.read().is_err(), "accepted invalid MBR at {offset}");
    }
}

#[test]
fn rejects_a_gpt_written_with_512_byte_blocks() {
    let mut bytes = Cursor::new(vec![0; SIZE]);
    gpt::mbr::ProtectiveMBR::with_lb_size((SIZE / 512 - 1) as u32)
        .overwrite_lba0(&mut bytes)
        .unwrap();
    let mut disk = gpt::GptConfig::new()
        .writable(true)
        .create_from_device(bytes, None)
        .unwrap();
    disk.add_partition(
        "root",
        1024 * 1024,
        partition_types::LINUX_ROOT_X64,
        0,
        Some(2048),
    )
    .unwrap();
    let mut bytes = disk.write().unwrap();
    assert!(Disk::read(&mut bytes, SIZE as u64, X64).is_err());
}

#[test]
fn mapping_conversion_rejects_overflow() {
    for (start, end) in [(u64::MAX, u64::MAX), (0, u64::MAX)] {
        let entry = Entry {
            number: 19,
            kind: X64,
            start,
            end,
            eligible: true,
        };
        let message = Partition::try_from(&entry).unwrap_err().to_string();
        assert!(message.contains("partition 19"));
        assert!(message.contains("512-byte device-mapper sectors"));
    }
}

impl Fixture {
    fn table_slots(&mut self, count: u32) {
        let mut primary = self.header(1);
        let mut backup = self.header(primary.backup_lba);
        let old = self.bytes.get_ref()[8192..8192 + primary.num_parts as usize * 128].to_vec();
        let blocks = (u64::from(count) * 128).div_ceil(4096);
        primary.num_parts = count;
        backup.num_parts = count;
        primary.first_usable = primary.part_start + blocks;
        backup.first_usable = primary.first_usable;
        backup.part_start = backup.current_lba - blocks;
        primary.last_usable = backup.part_start - 1;
        backup.last_usable = primary.last_usable;
        for start in [primary.part_start, backup.part_start] {
            let start = start as usize * 4096;
            self.bytes.get_mut()[start..start + blocks as usize * 4096].fill(0);
            self.bytes.get_mut()[start..start + old.len()].copy_from_slice(&old);
        }
        primary
            .write_primary(&mut self.bytes, GPT_BLOCK_SIZE)
            .unwrap();
        backup
            .write_backup(&mut self.bytes, GPT_BLOCK_SIZE)
            .unwrap();
    }
}

#[derive(Debug)]
struct ReadSpy {
    bytes: Cursor<Vec<u8>>,
    reads: Vec<u64>,
    fail_at: Option<u64>,
}

impl Read for ReadSpy {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        assert!(buffer.len() <= 4096, "reader requested a complete table");
        let position = self.bytes.position();
        self.reads.push(position);
        if self.fail_at == Some(position) {
            return Err(io::Error::other("injected device failure"));
        }
        self.bytes.read(buffer)
    }
}

impl Seek for ReadSpy {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.bytes.seek(position)
    }
}

#[test]
fn streams_both_tables_to_the_end_with_bounded_reads() {
    let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
    fixture.entries(|entries| {
        entries.get_mut(&1).unwrap().first_lba = 512;
        entries.get_mut(&1).unwrap().last_lba = 767;
    });
    fixture.table_slots(MAX_ENTRIES);
    let primary = fixture.header(1);
    let backup = fixture.header(primary.backup_lba);
    let mut spy = ReadSpy {
        bytes: fixture.bytes,
        reads: Vec::new(),
        fail_at: None,
    };
    assert_eq!(
        Disk::read(&mut spy, carapace::ZERO_SIZE_BYTES, X64)
            .unwrap()
            .root
            .start,
        4096
    );
    let mut expected = vec![0, 4096, (SIZE - 4096) as u64];
    for start in [primary.part_start, backup.part_start] {
        expected.extend((0..256).map(|index| (start + index) * 4096));
    }
    spy.reads.sort_unstable();
    expected.sort_unstable();
    assert_eq!(
        spy.reads, expected,
        "must read only metadata, including every table block"
    );
    let primary = Disk::header(&mut spy, 1, "primary").unwrap();
    let backup = Disk::header(&mut spy, primary.backup_lba, "backup").unwrap();
    let entries = Disk::entries(&mut spy, &primary, &backup).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries.capacity() < 128,
        "unused slots must not reserve records"
    );
}

#[test]
fn ignores_unused_entries_with_nonzero_fields() {
    let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
    let expected = fixture.read().unwrap();
    fixture.entries(|entries| {
        let mut unused = entries[&1].clone();
        unused.part_type_guid.guid = Uuid::nil();
        unused.first_lba = u64::MAX;
        unused.last_lba = 0;
        entries.insert(2, unused);
    });
    let actual = fixture.read().unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn accepts_sparse_reordered_entries_and_ignores_partition_identity() {
    let mut fixture = Fixture::new(&[
        partition_types::LINUX_ROOT_X64,
        partition_types::EFI,
        partition_types::LINUX_SWAP,
        partition_types::LINUX_ROOT_ARM_64,
    ]);
    fixture.entries(|entries| {
        let root = entries.remove(&1).unwrap();
        entries.insert(121, root);
        let esp = entries.remove(&2).unwrap();
        entries.insert(37, esp);
        let root_id = entries[&121].part_guid;
        entries.get_mut(&37).unwrap().part_guid = root_id;
        let extra = entries.get_mut(&3).unwrap();
        extra.part_guid = "00000000-0000-0000-0000-000000000000".parse().unwrap();
        extra.part_type_guid.guid = "11111111-2222-3333-4444-555555555555".parse().unwrap();
    });
    assert_eq!(fixture.read().unwrap().start, 2048);
    assert_eq!(fixture.read_as(ARM64).unwrap().start, 8192);
}

#[test]
fn rejects_a_second_root_in_the_last_table_slot() {
    let mut fixture = Fixture::new(&[
        partition_types::LINUX_ROOT_X64,
        partition_types::LINUX_ROOT_X64,
    ]);
    fixture.entries(|entries| {
        let second = entries.remove(&2).unwrap();
        entries.insert(128, second);
    });
    let message = fixture.read().unwrap_err().to_string();
    assert!(message.contains("multiple discoverable root"), "{message}");
    assert!(
        message.contains("partition 1 ") && message.contains("partition 128 "),
        "{message}"
    );
}

#[test]
fn checks_overlaps_between_non_root_entries_and_preserves_table_numbers() {
    for mode in 0..3 {
        let mut fixture = Fixture::new(&[
            partition_types::LINUX_ROOT_X64,
            partition_types::EFI,
            partition_types::LINUX_SWAP,
        ]);
        fixture.entries(|entries| {
            let esp = entries.remove(&2).unwrap();
            let mut swap = entries.remove(&3).unwrap();
            match mode {
                0 => swap.first_lba = esp.last_lba + 1, // valid adjacency
                1 => swap.first_lba = esp.last_lba,     // one shared block
                2 => {
                    swap.first_lba = esp.first_lba + 1;
                    swap.last_lba = esp.last_lba - 1;
                }
                _ => unreachable!(),
            }
            entries.insert(117, esp);
            entries.insert(38, swap);
        });
        if mode == 0 {
            assert!(fixture.read().is_ok());
        } else {
            let message = fixture.read().unwrap_err().to_string();
            for part in [
                "partition 117",
                "partition 38",
                "overlaps",
                "4096-byte logical blocks",
            ] {
                assert!(message.contains(part), "{message}");
            }
        }
    }
}

#[test]
fn reports_bounds_with_partition_number_and_units() {
    let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64, partition_types::EFI]);
    fixture.entries(|entries| entries.get_mut(&2).unwrap().last_lba = 2047);
    let message = fixture.read().unwrap_err().to_string();
    for part in [
        "partition 2",
        "2047",
        "outside usable GPT LBAs",
        "4096-byte logical blocks",
    ] {
        assert!(message.contains(part), "{message}");
    }
}

#[test]
fn verifies_crcs_after_the_root_and_names_the_bad_copy() {
    for copy in ["primary", "backup"] {
        let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
        let start = if copy == "primary" {
            8192
        } else {
            SIZE - 5 * 4096
        };
        fixture.bytes.get_mut()[start + 127 * 128 + 56] ^= 1;
        let message = fixture.read().unwrap_err().to_string();
        assert!(
            message.contains(&format!("{copy} GPT partition-array checksum mismatch")),
            "{message}"
        );
        assert!(
            message.contains("expected 0x") && message.contains("calculated 0x"),
            "{message}"
        );
    }
}

#[test]
fn rejects_different_tables_even_when_their_crcs_match() {
    let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
    // Equal-length CRC32 collision: place it in an unused entry's ignored name.
    let crc = Crc::<u32>::new(&CRC_32_ISO_HDLC);
    assert_eq!(crc.checksum(b"plumless"), crc.checksum(b"buckeroo"));
    for (start, name) in [(8192, b"plumless"), (SIZE - 5 * 4096, b"buckeroo")] {
        let offset = start + 127 * 128 + 56;
        fixture.bytes.get_mut()[offset..offset + 8].copy_from_slice(name);
    }
    fixture.headers(|_, _| {});
    assert_eq!(
        fixture.header(1).crc32_parts,
        fixture.header((SIZE / 4096 - 1) as u64).crc32_parts
    );
    let message = fixture.read().unwrap_err().to_string();
    assert!(message.contains("partition tables disagree"), "{message}");
    assert!(message.contains("partition 128"), "{message}");
}

#[test]
fn excludes_padding_after_the_declared_table_from_crc_and_comparison() {
    let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
    fixture.table_slots(129);
    assert!(fixture.read().is_ok());
    let offset = 8192 + 129 * 128;
    fixture.bytes.get_mut()[offset] = 0xaa;
    assert!(
        fixture.read().is_ok(),
        "table block padding is not a partition entry"
    );
    fixture.bytes.get_mut()[offset - 1] = 0xaa;
    assert!(
        fixture
            .read()
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch")
    );
}

#[test]
fn reports_read_location_and_cause() {
    let fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
    let mut spy = ReadSpy {
        bytes: fixture.bytes,
        reads: Vec::new(),
        fail_at: Some(8192 + 4096),
    };
    let error = Disk::read(&mut spy, SIZE as u64, X64).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    let message = error.to_string();
    for part in [
        "primary GPT table",
        "LBA 3",
        "byte offset 12288",
        "injected device failure",
    ] {
        assert!(message.contains(part), "{message}");
    }
}

#[test]
fn rejects_excessive_table_work_before_reading_entries() {
    let mut fixture = Fixture::new(&[partition_types::LINUX_ROOT_X64]);
    assert!(fixture.read().is_ok());
    let mut primary = fixture.header(1);
    primary.num_parts = MAX_ENTRIES + 1;
    primary
        .write_primary(&mut fixture.bytes, GPT_BLOCK_SIZE)
        .unwrap();
    let mut spy = ReadSpy {
        bytes: fixture.bytes,
        reads: Vec::new(),
        fail_at: None,
    };
    let message = Disk::read(&mut spy, SIZE as u64, X64)
        .unwrap_err()
        .to_string();
    assert!(message.contains("geometry"), "{message}");
    assert_eq!(spy.reads, [4096]);
}
