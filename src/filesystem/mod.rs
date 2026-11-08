// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Identify common root formats before invoking a filesystem driver.

use std::io::{self, Read, Seek, SeekFrom};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Filesystem {
    Ext,
    Xfs,
    Btrfs,
}

impl Filesystem {
    /// Inspect the primary superblocks only. Checksums and feature validation
    /// remain the kernel driver's job; a failed recognized mount is fatal.
    pub(crate) fn probe(mut source: impl Read + Seek) -> io::Result<Option<Self>> {
        let signatures: [(Self, u64, &[u8]); 3] = [
            (Self::Ext, 1024 + 0x38, &[0x53, 0xef]),
            (Self::Xfs, 0, b"XFSB"),
            (Self::Btrfs, 0x10000 + 0x40, b"_BHRfS_M"),
        ];
        let mut found = None;
        for (filesystem, offset, magic) in signatures {
            source.seek(SeekFrom::Start(offset))?;
            let mut bytes = [0; 8];
            match source.read_exact(&mut bytes[..magic.len()]) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => continue,
                Err(error) => return Err(error),
            }
            if &bytes[..magic.len()] == magic && found.replace(filesystem).is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "conflicting root filesystem signatures",
                ));
            }
        }
        Ok(found)
    }

    pub(crate) fn driver(self, registered: &str) -> Result<&'static str, String> {
        let candidates: &[&str] = match self {
            // ext4 also accepts ext2/ext3. Older kernels may expose only the
            // older drivers, whose feature checks still reject newer formats.
            Self::Ext => &["ext4", "ext3", "ext2"],
            Self::Xfs => &["xfs"],
            Self::Btrfs => &["btrfs"],
        };
        candidates
            .iter()
            .copied()
            .find(|candidate| registered.lines().any(|line| line.trim() == *candidate))
            .ok_or_else(|| format!("no registered driver for {self:?} root filesystem"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn recognizes_primary_superblock_signatures() {
        for (expected, offset, magic) in [
            (Filesystem::Ext, 1080, &[0x53, 0xef][..]),
            (Filesystem::Xfs, 0, b"XFSB".as_slice()),
            (Filesystem::Btrfs, 65600, b"_BHRfS_M".as_slice()),
        ] {
            let mut bytes = vec![0; 69632];
            bytes[offset..offset + magic.len()].copy_from_slice(magic);
            assert_eq!(
                Filesystem::probe(Cursor::new(&bytes)).unwrap(),
                Some(expected)
            );
            bytes[offset] ^= 1;
            assert_eq!(Filesystem::probe(Cursor::new(&bytes)).unwrap(), None);
        }
    }

    #[test]
    fn rejects_conflicting_signatures_and_does_not_invent_a_type() {
        let mut bytes = vec![0; 2048];
        bytes[..4].copy_from_slice(b"XFSB");
        bytes[1080..1082].copy_from_slice(&[0x53, 0xef]);
        assert_eq!(
            Filesystem::probe(Cursor::new(bytes)).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        for size in [0, 3, 1081, 65536] {
            assert_eq!(Filesystem::probe(Cursor::new(vec![0; size])).unwrap(), None);
        }
    }

    #[test]
    fn requires_the_matching_registered_block_driver() {
        let drivers = "nodev\tproc\next3\next2\nxfs\next4\nbtrfs\n";
        for (filesystem, driver) in [
            (Filesystem::Ext, "ext4"),
            (Filesystem::Xfs, "xfs"),
            (Filesystem::Btrfs, "btrfs"),
        ] {
            assert_eq!(filesystem.driver(drivers).unwrap(), driver);
            assert!(
                filesystem
                    .driver("nodev ext4\nnodev xfs\nnodev btrfs\n")
                    .is_err()
            );
        }
        assert_eq!(Filesystem::Ext.driver("ext2\next3\n").unwrap(), "ext3");
    }
}
