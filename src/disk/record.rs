// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::little_endian::U64};

/// GPT 1.0 entry. Identity and name remain opaque; the table CRC covers them.
#[derive(Debug, FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(super) struct Record {
    pub(super) kind: [u8; 16],
    identity: [u8; 16],
    pub(super) start: U64,
    pub(super) end: U64,
    pub(super) attributes: U64,
    name: [u8; 72],
}

const _: () = assert!(size_of::<Record>() == 128 && align_of::<Record>() == 1);
const _: () = assert!(std::mem::offset_of!(Record, start) == 32);
const _: () = assert!(std::mem::offset_of!(Record, attributes) == 48);
const _: () = assert!(std::mem::offset_of!(Record, name) == 56);

impl Record {
    pub(super) fn is_used(&self) -> bool {
        self.kind != [0; 16]
    }
}
