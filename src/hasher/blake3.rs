use crate::{Digest, Hasher};

/// BLAKE3, the default hasher when the `hasher-blake3` feature is on.
#[derive(Default)]
pub struct Blake3(::blake3::Hasher);

impl Hasher for Blake3 {
    type Output = Digest<32>;

    const LEN: usize = 32;
    const NAME: &'static str = "blake3";

    fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finalize(self) -> Self::Output {
        Digest::from_bytes(*self.0.finalize().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::hasher::check_vector};

    #[test]
    fn matches_the_published_test_vector() {
        check_vector::<Blake3>(
            b"abc",
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85",
        );
    }
}
