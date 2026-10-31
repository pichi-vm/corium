// SPDX-License-Identifier: Apache-2.0

use std::io;

// Keep RustCrypto trait generations behind a private interface. The tree and
// salt-ordering code need only these three operations, not a public digest API.
pub(crate) trait Hasher: Send + Sync {
    fn output_size(&self) -> usize;
    fn update(&mut self, data: &[u8]);
    fn finalize_into_reset(&mut self, output: &mut [u8]) -> io::Result<()>;
}

#[cfg(any(
    feature = "sha1",
    feature = "ripemd",
    feature = "whirlpool",
    feature = "streebog",
    feature = "sm3",
    feature = "blake2"
))]
struct Legacy<T>(T);

#[cfg(any(
    feature = "sha1",
    feature = "ripemd",
    feature = "whirlpool",
    feature = "streebog",
    feature = "sm3",
    feature = "blake2"
))]
pub(crate) fn legacy<T: digest::DynDigest + Send + Sync + 'static>(hasher: T) -> Box<dyn Hasher> {
    Box::new(Legacy(hasher))
}

#[cfg(any(
    feature = "sha1",
    feature = "ripemd",
    feature = "whirlpool",
    feature = "streebog",
    feature = "sm3",
    feature = "blake2"
))]
impl<T: digest::DynDigest + Send + Sync> Hasher for Legacy<T> {
    fn output_size(&self) -> usize {
        self.0.output_size()
    }

    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    fn finalize_into_reset(&mut self, output: &mut [u8]) -> io::Result<()> {
        self.0.finalize_into_reset(output).map_err(|_| {
            io::Error::other("hash output buffer does not match the algorithm's digest size")
        })
    }
}

#[cfg(any(feature = "sha2", feature = "sha3"))]
struct Modern<T>(T);

#[cfg(any(feature = "sha2", feature = "sha3"))]
pub(crate) fn modern<T: digest_next::DynDigest + Send + Sync + 'static>(
    hasher: T,
) -> Box<dyn Hasher> {
    Box::new(Modern(hasher))
}

#[cfg(any(feature = "sha2", feature = "sha3"))]
impl<T: digest_next::DynDigest + Send + Sync> Hasher for Modern<T> {
    fn output_size(&self) -> usize {
        self.0.output_size()
    }

    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    fn finalize_into_reset(&mut self, output: &mut [u8]) -> io::Result<()> {
        self.0.finalize_into_reset(output).map_err(|_| {
            io::Error::other("hash output buffer does not match the algorithm's digest size")
        })
    }
}

#[cfg(all(test, feature = "sha2"))]
mod tests {
    use crate::Algorithm;

    #[test]
    fn hasher_preserves_incremental_input_and_reset() {
        let mut hasher = Algorithm::Sha256.hasher().unwrap();
        assert_eq!(hasher.output_size(), 32);
        hasher.update(b"a");
        hasher.update(b"bc");
        let mut output = [0; 32];
        hasher.finalize_into_reset(&mut output).unwrap();
        assert_eq!(
            output,
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
        hasher.update(b"abc");
        let mut repeated = [0; 32];
        hasher.finalize_into_reset(&mut repeated).unwrap();
        assert_eq!(repeated, output);
    }

    #[test]
    fn wrong_output_size_is_an_io_error() {
        let mut hasher = Algorithm::Sha256.hasher().unwrap();
        assert_eq!(
            hasher.finalize_into_reset(&mut [0; 31]).unwrap_err().kind(),
            std::io::ErrorKind::Other
        );
    }
}

#[cfg(all(test, feature = "sha3"))]
mod modern_tests {
    use crate::Algorithm;

    #[test]
    fn modern_hasher_resets_and_rejects_wrong_output_sizes() {
        let mut hasher = Algorithm::Sha3_256.hasher().unwrap();
        assert_eq!(hasher.output_size(), 32);
        assert!(hasher.finalize_into_reset(&mut [0; 31]).is_err());
        hasher.update(b"a");
        hasher.update(b"bc");
        let mut output = [0; 32];
        hasher.finalize_into_reset(&mut output).unwrap();
        hasher.update(b"abc");
        let mut repeated = [0; 32];
        hasher.finalize_into_reset(&mut repeated).unwrap();
        assert_eq!(repeated, output);
        let mut fresh = Algorithm::Sha3_256.hasher().unwrap();
        fresh.update(b"abc");
        fresh.finalize_into_reset(&mut repeated).unwrap();
        assert_eq!(repeated, output);
    }
}
