/// BLAKE3.
#[cfg(feature = "hasher-blake3")]
mod blake3;
/// The fixed-size digest type, and the trait every hash output implements.
mod digest;
/// SHA-256.
#[cfg(feature = "hasher-sha256")]
mod sha256;

#[cfg(feature = "hasher-blake3")]
pub use self::blake3::*;
#[cfg(feature = "hasher-sha256")]
pub use self::sha256::*;
pub use digest::*;

/// A hash function that names blobs.
///
/// The crate does not ship a hasher with known collision attacks, such as
/// MD5 or SHA-1: with one, two different files could share one name.
pub trait Hasher: Default + Send + 'static {
    /// The name written to the store's layout file.
    const NAME: &'static str;

    /// The digest length, in bytes.
    const LEN: usize;

    type Output: HashOutput;

    fn update(&mut self, bytes: &[u8]);

    fn finalize(self) -> Self::Output;

    /// The digest of `bytes` in one call.
    fn digest(bytes: &[u8]) -> Self::Output {
        let mut hasher = Self::default();
        hasher.update(bytes);
        hasher.finalize()
    }
}

/// Check a hasher against a published test vector: in one call, and fed in
/// chunks of several sizes.
#[cfg(all(test, any(feature = "hasher-blake3", feature = "hasher-sha256")))]
pub(crate) fn check_vector<H: Hasher>(input: &[u8], expected: &str) {
    assert_eq!(H::digest(input).to_string(), expected);
    assert_eq!(H::digest(input).as_ref().len(), H::LEN);
    for size in [1, 3, 64] {
        let mut hasher = H::default();
        for chunk in input.chunks(size) {
            hasher.update(chunk);
        }
        assert_eq!(hasher.finalize().to_string(), expected, "chunks of {size}");
    }
}
