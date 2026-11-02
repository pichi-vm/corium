// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use super::Disk;
use crc::{CRC_32_ISO_HDLC, Crc};
use std::io;
use zerocopy::{
    FromBytes, Immutable, KnownLayout, Unaligned,
    byteorder::little_endian::{U32, U64},
};

/// GPT 1.0 header. Fixed geometry is checked before using any disk lengths.
#[derive(Debug, FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(super) struct Header {
    signature: [u8; 8],
    revision: U32,
    size: U32,
    crc32: U32,
    reserved: U32,
    pub(super) current_lba: U64,
    pub(super) backup_lba: U64,
    pub(super) first_usable: U64,
    pub(super) last_usable: U64,
    pub(super) disk_guid: [u8; 16],
    pub(super) part_start: U64,
    pub(super) num_parts: U32,
    part_size: U32,
    pub(super) crc32_parts: U32,
}

const _: () = assert!(size_of::<Header>() == 92 && align_of::<Header>() == 1);
const _: () = assert!(std::mem::offset_of!(Header, crc32) == 16);
const _: () = assert!(std::mem::offset_of!(Header, current_lba) == 24);
const _: () = assert!(std::mem::offset_of!(Header, crc32_parts) == 88);

impl Header {
    pub(super) fn read(bytes: &[u8; 4096], context: &str) -> io::Result<Self> {
        let header = Self::read_from_bytes(&bytes[..92]).expect("92-byte, alignment-one header");
        if header.signature != *b"EFI PART" {
            return Err(Disk::invalid(format!("{context}: invalid GPT signature")));
        }
        if header.revision.get() != 0x0001_0000
            || header.size.get() != 92
            || header.reserved.get() != 0
            || header.part_size.get() != 128
            || !(128..=super::MAX_ENTRIES).contains(&header.num_parts.get())
            || header.disk_guid == [0; 16]
            || bytes[92..].iter().any(|byte| *byte != 0)
        {
            return Err(Disk::invalid(format!(
                "{context}: unsupported header or entry geometry: {header:?}; requires GPT 1.0, a 92-byte header, 128-byte entries, 128..8192 slots, a nonzero disk UUID and zero reserved bytes"
            )));
        }
        let crc = Crc::<u32>::new(&CRC_32_ISO_HDLC);
        let mut digest = crc.digest();
        digest.update(&bytes[..16]);
        digest.update(&[0; 4]);
        digest.update(&bytes[20..92]);
        let calculated = digest.finalize();
        let expected = header.crc32.get();
        if calculated != expected {
            return Err(Disk::invalid(format!(
                "{context} checksum mismatch: expected {expected:#010x}, calculated {calculated:#010x}"
            )));
        }
        Ok(header)
    }
}
