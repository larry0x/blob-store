use std::{
    fmt::{self, Debug, Display},
    hash::Hash,
    str::FromStr,
};

/// What a [`crate::Hasher`] produces: a fixed-size digest, shown as lowercase
/// hex.
pub trait HashOutput:
    Copy
    + Eq
    + Ord
    + Hash
    + Debug
    + Display
    + FromStr<Err = ParseDigestError>
    + AsRef<[u8]>
    + Send
    + Sync
    + 'static
{
    /// The digest of these bytes, or `None` when the length is wrong.
    fn from_slice(bytes: &[u8]) -> Option<Self>;
}

/// A digest of `L` bytes.
///
/// It displays as `2 * L` lowercase hex characters. Parsing accepts either
/// case.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest<const L: usize>([u8; L]);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseDigestError {
    #[error("expected {expected} hex characters, got {found}")]
    Length { expected: usize, found: usize },
    #[error("invalid hex character {character:?} at position {index}")]
    Hex { character: char, index: usize },
}

impl<const L: usize> Digest<L> {
    pub const fn from_bytes(bytes: [u8; L]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; L] {
        &self.0
    }

    pub const fn into_bytes(self) -> [u8; L] {
        self.0
    }
}

impl<const L: usize> HashOutput for Digest<L> {
    fn from_slice(bytes: &[u8]) -> Option<Self> {
        bytes.try_into().ok().map(Self)
    }
}

impl<const L: usize> AsRef<[u8]> for Digest<L> {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl<const L: usize> FromStr for Digest<L> {
    type Err = ParseDigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 2 * L {
            return Err(ParseDigestError::Length {
                expected: 2 * L,
                found: s.len(),
            });
        }
        let mut bytes = [0; L];
        hex::decode_to_slice(s, &mut bytes).map_err(|error| match error {
            hex::FromHexError::InvalidHexCharacter { c, index } => ParseDigestError::Hex {
                character: c,
                index,
            },
            // The length is checked above, so only a bad character remains.
            hex::FromHexError::OddLength | hex::FromHexError::InvalidStringLength => {
                ParseDigestError::Length {
                    expected: 2 * L,
                    found: s.len(),
                }
            },
        })?;
        Ok(Self(bytes))
    }
}

impl<const L: usize> Display for Digest<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buffer = vec![0; 2 * L];
        hex::encode_to_slice(self.0, &mut buffer)
            .expect("the buffer holds two characters per byte");
        f.write_str(std::str::from_utf8(&buffer).expect("hex is ASCII"))
    }
}

impl<const L: usize> Debug for Digest<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({self})")
    }
}

#[cfg(feature = "serde")]
impl<const L: usize> serde::Serialize for Digest<L> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(feature = "serde")]
impl<'de, const L: usize> serde::Deserialize<'de> for Digest<L> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type D4 = Digest<4>;

    #[test]
    fn displays_lowercase_hex() {
        let digest = D4::from_bytes([0x01, 0xab, 0xcd, 0xef]);
        assert_eq!(digest.to_string(), "01abcdef");
        assert_eq!(format!("{digest:?}"), "Digest(01abcdef)");
    }

    #[test]
    fn parses_either_case_to_the_same_value() {
        let lower: D4 = "01abcdef".parse().unwrap();
        let upper: D4 = "01ABCDEF".parse().unwrap();
        assert_eq!(lower, upper);
        assert_eq!(lower.as_bytes(), &[0x01, 0xab, 0xcd, 0xef]);
        assert_eq!(upper.to_string(), "01abcdef");
    }

    #[test]
    fn rejects_the_wrong_length() {
        for input in ["", "01abcde", "01abcdef0", "01abcdef01"] {
            assert_eq!(
                input.parse::<D4>(),
                Err(ParseDigestError::Length {
                    expected: 8,
                    found: input.len()
                }),
                "{input:?}"
            );
        }
    }

    #[test]
    fn rejects_non_hex() {
        assert_eq!(
            "01abcdeg".parse::<D4>(),
            Err(ParseDigestError::Hex {
                character: 'g',
                index: 7
            })
        );
        // A multi-byte character can give the right byte length.
        assert!("01abcdé".parse::<D4>().is_err());
    }

    #[test]
    fn from_slice_checks_the_length() {
        assert_eq!(
            D4::from_slice(&[1, 2, 3, 4]),
            Some(D4::from_bytes([1, 2, 3, 4]))
        );
        assert_eq!(D4::from_slice(&[1, 2, 3]), None);
        assert_eq!(D4::from_slice(&[1, 2, 3, 4, 5]), None);
    }

    #[test]
    fn bytes_round_trip() {
        let digest = D4::from_bytes([9, 8, 7, 6]);
        assert_eq!(D4::from_bytes(digest.into_bytes()), digest);
        assert_eq!(digest.as_ref(), &[9, 8, 7, 6]);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_uses_the_hex_string() {
        let digest = D4::from_bytes([0x0f, 0x0f, 0x0f, 0x0f]);
        let json = serde_json::to_string(&digest).unwrap();
        assert_eq!(json, "\"0f0f0f0f\"");
        assert_eq!(serde_json::from_str::<D4>(&json).unwrap(), digest);
        assert!(serde_json::from_str::<D4>("\"zz\"").is_err());
    }
}
