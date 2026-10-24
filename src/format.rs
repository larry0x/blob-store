/// The `formats!` macro, which defines a format enum out of presets.
#[cfg(feature = "format-presets")]
mod r#macro;
/// Ready-made formats: a unit type for each, the `Preset` trait they share,
/// and `identify`, which tells them apart.
#[cfg(feature = "format-presets")]
mod presets;

#[cfg(feature = "format-presets")]
pub use presets::*;
use std::fmt::Debug;

/// The file formats a store accepts. An app defines one enum with every
/// format it takes, and one store holds blobs of all of them.
///
/// The format comes from the content, never from the caller, so a hash
/// always maps to one path.
///
/// With the `format-presets` feature, the crate defines the common formats,
/// and [`crate::formats!`] builds such an enum out of them.
pub trait Format: Copy + Eq + Debug + Send + Sync + 'static {
    /// Identify a file by its first bytes: up to [`crate::SNIFF_LEN`] of them,
    /// or the whole file when it is shorter. `None` rejects the file.
    fn sniff(head: &[u8]) -> Option<Self>;

    /// The file extension, without the dot. It must be unique per format,
    /// and contain no dot.
    fn extension(&self) -> &'static str;

    /// The inverse of [`Format::extension`].
    fn from_extension(extension: &str) -> Option<Self>;
}
