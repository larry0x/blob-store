use crate::{Digest, Hasher};

/// SHA-256.
#[derive(Default)]
pub struct Sha256(sha2::Sha256);

impl Hasher for Sha256 {
    type Output = Digest<32>;

    const LEN: usize = 32;
    const NAME: &'static str = "sha256";

    fn update(&mut self, bytes: &[u8]) {
        sha2::Digest::update(&mut self.0, bytes);
    }

    fn finalize(self) -> Self::Output {
        Digest::from_bytes(sha2::Digest::finalize(self.0).into())
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::hasher::check_vector};

    #[test]
    fn matches_the_published_test_vector() {
        check_vector::<Sha256>(
            b"abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
    }
}
