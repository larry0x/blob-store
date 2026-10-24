use crate::Format;

/// A file format the crate knows: its extension, its media type, and its
/// signature.
///
/// Each preset is a [`Format`] on its own, for a store that takes one
/// format. For a store that takes several, list them in
/// [`crate::formats!`].
pub trait Preset {
    /// The file extension, without the dot.
    const EXTENSION: &'static str;

    /// The media type, as an HTTP response or a data URI names it.
    const MEDIA_TYPE: &'static str;

    /// Whether `head` carries this format's signature. Two signatures can
    /// both match one file; [`identify`] settles which format it is.
    fn matches(head: &[u8]) -> bool;
}

/// Defines each preset, and [`identify`] with the presets in the order given.
macro_rules! presets {
    ($(
        $(#[$doc:meta])*
        $name:ident, $extension:literal, $media_type:literal, $matches:path;
    )+) => {
        $(
            $(#[$doc])*
            #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
            pub struct $name;

            impl Preset for $name {
                const EXTENSION: &'static str = $extension;
                const MEDIA_TYPE: &'static str = $media_type;

                fn matches(head: &[u8]) -> bool {
                    $matches(head)
                }
            }

            impl Format for $name {
                fn sniff(head: &[u8]) -> Option<Self> {
                    (identify(head) == Some($extension)).then_some(Self)
                }

                fn extension(&self) -> &'static str {
                    $extension
                }

                fn from_extension(extension: &str) -> Option<Self> {
                    (extension == $extension).then_some(Self)
                }
            }
        )+

        /// Which preset a file is, by its first bytes. Returns the preset's
        /// extension, or `None` when no preset matches.
        ///
        /// Signatures at a fixed offset come first, the MP4 and QuickTime
        /// boxes next, and PDF last: a PDF header may sit anywhere in the
        /// first 1,024 bytes, so a PNG or a gzip file can contain one by
        /// chance.
        pub fn identify(head: &[u8]) -> Option<&'static str> {
            $(
                if $matches(head) {
                    return Some($extension);
                }
            )+
            None
        }
    };
}

presets! {
    /// JPEG, stored as `.jpg`.
    Jpeg, "jpg", "image/jpeg", infer::image::is_jpeg;
    /// PNG.
    Png, "png", "image/png", infer::image::is_png;
    /// GIF, still or animated.
    Gif, "gif", "image/gif", infer::image::is_gif;
    /// WebP.
    Webp, "webp", "image/webp", infer::image::is_webp;
    /// WebM. The signature needs more than 256 bytes.
    Webm, "webm", "video/webm", infer::video::is_webm;
    /// MP4: an ISO media file with an MP4 brand.
    Mp4, "mp4", "video/mp4", infer::video::is_mp4;
    /// QuickTime. Phones write it under an `.mp4` name, but its `ftyp` brand
    /// is `qt  `, so it is a format of its own.
    Mov, "mov", "video/quicktime", infer::video::is_mov;
    /// gzip, stored as `.gz`.
    Gzip, "gz", "application/gzip", infer::archive::is_gz;
    /// PDF.
    Pdf, "pdf", "application/pdf", infer::archive::is_pdf;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each sample is the smallest head that carries its format's signature.

    fn jpeg() -> Vec<u8> {
        vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00,
        ]
    }

    fn png() -> Vec<u8> {
        vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D,
        ]
    }

    fn gif() -> Vec<u8> {
        b"GIF89a\x01\x00\x01\x00\x00\x00\x00".to_vec()
    }

    fn webp() -> Vec<u8> {
        b"RIFF\x24\x00\x00\x00WEBPVP8 ".to_vec()
    }

    /// An EBML header whose DocType is `doc_type`, padded past 256 bytes.
    fn ebml(doc_type: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0x1A, 0x45, 0xDF, 0xA3, 0x9F];
        bytes.extend([0x42, 0x82, doc_type.len() as u8 | 0x80]);
        bytes.extend(doc_type);
        bytes.resize(300, 0);
        bytes
    }

    /// An ISO media file that opens with an `ftyp` box of this brand.
    fn ftyp(brand: &[u8; 4]) -> Vec<u8> {
        let mut bytes = vec![0x00, 0x00, 0x00, 0x18];
        bytes.extend(b"ftyp");
        bytes.extend(brand);
        bytes.extend([0x00, 0x00, 0x02, 0x00]);
        bytes.extend(brand);
        bytes.extend(b"mp41");
        bytes
    }

    fn gzip() -> Vec<u8> {
        vec![0x1F, 0x8B, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03]
    }

    fn pdf() -> Vec<u8> {
        b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec()
    }

    fn samples() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("jpg", jpeg()),
            ("png", png()),
            ("gif", gif()),
            ("webp", webp()),
            ("webm", ebml(b"webm")),
            ("mp4", ftyp(b"isom")),
            ("mov", ftyp(b"qt  ")),
            ("gz", gzip()),
            ("pdf", pdf()),
        ]
    }

    /// The extension each preset's `Format` impl claims, by sniffing.
    fn sniffed(head: &[u8]) -> Vec<&'static str> {
        [
            Jpeg::sniff(head).map(|f| f.extension()),
            Png::sniff(head).map(|f| f.extension()),
            Gif::sniff(head).map(|f| f.extension()),
            Webp::sniff(head).map(|f| f.extension()),
            Webm::sniff(head).map(|f| f.extension()),
            Mp4::sniff(head).map(|f| f.extension()),
            Mov::sniff(head).map(|f| f.extension()),
            Gzip::sniff(head).map(|f| f.extension()),
            Pdf::sniff(head).map(|f| f.extension()),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    #[test]
    fn each_sample_is_identified_as_its_own_format() {
        for (extension, head) in samples() {
            assert_eq!(identify(&head), Some(extension), "{extension}");
        }
    }

    #[test]
    fn exactly_one_preset_claims_each_sample() {
        for (extension, head) in samples() {
            assert_eq!(sniffed(&head), [extension], "{extension}");
        }
    }

    #[test]
    fn quicktime_and_mp4_are_told_apart_by_the_brand() {
        assert_eq!(identify(&ftyp(b"qt  ")), Some("mov"));
        for brand in [b"isom", b"mp41", b"mp42", b"avc1", b"iso5"] {
            assert_eq!(identify(&ftyp(brand)), Some("mp4"));
        }
        // A QuickTime file from before `ftyp`: it opens with a `moov` box.
        let mut legacy = vec![0x00, 0x00, 0x10, 0x00];
        legacy.extend(b"moov");
        legacy.resize(32, 0);
        assert_eq!(identify(&legacy), Some("mov"));
    }

    #[test]
    fn a_pdf_header_inside_another_format_does_not_win() {
        for (extension, mut head) in [("png", png()), ("gz", gzip()), ("jpg", jpeg())] {
            head.extend(b"text %PDF-1.7 text");
            assert_eq!(identify(&head), Some(extension), "{extension}");
            assert_eq!(Pdf::sniff(&head), None, "{extension}");
        }
    }

    #[test]
    fn a_pdf_header_may_follow_leading_bytes() {
        let mut head = b"junk before the header\n".to_vec();
        head.extend(pdf());
        assert_eq!(identify(&head), Some("pdf"));
    }

    #[test]
    fn other_formats_and_noise_are_not_identified() {
        let mut m4a = ftyp(b"M4A ");
        m4a.truncate(16);
        for head in [
            Vec::new(),
            b"just some text".to_vec(),
            b"BM\x00\x00\x00\x00\x00\x00".to_vec(), // BMP
            ebml(b"matroska"),                      // MKV
            m4a,
            b"PK\x03\x04".to_vec(), // zip
        ] {
            assert_eq!(identify(&head), None, "{head:?}");
            assert!(sniffed(&head).is_empty(), "{head:?}");
        }
    }

    #[test]
    fn a_short_webm_head_is_not_identified() {
        let mut head = ebml(b"webm");
        head.truncate(256);
        assert_eq!(identify(&head), None);
    }

    #[test]
    fn extensions_and_media_types_are_the_canonical_ones() {
        let table = [
            (Jpeg::EXTENSION, Jpeg::MEDIA_TYPE, "jpg", "image/jpeg"),
            (Png::EXTENSION, Png::MEDIA_TYPE, "png", "image/png"),
            (Gif::EXTENSION, Gif::MEDIA_TYPE, "gif", "image/gif"),
            (Webp::EXTENSION, Webp::MEDIA_TYPE, "webp", "image/webp"),
            (Webm::EXTENSION, Webm::MEDIA_TYPE, "webm", "video/webm"),
            (Mp4::EXTENSION, Mp4::MEDIA_TYPE, "mp4", "video/mp4"),
            (Mov::EXTENSION, Mov::MEDIA_TYPE, "mov", "video/quicktime"),
            (Gzip::EXTENSION, Gzip::MEDIA_TYPE, "gz", "application/gzip"),
            (Pdf::EXTENSION, Pdf::MEDIA_TYPE, "pdf", "application/pdf"),
        ];
        for (extension, media_type, expected_extension, expected_media_type) in table {
            assert_eq!(extension, expected_extension);
            assert_eq!(media_type, expected_media_type);
        }
    }

    #[test]
    fn a_preset_parses_only_its_own_extension() {
        assert_eq!(Jpeg::from_extension("jpg"), Some(Jpeg));
        assert_eq!(Jpeg::from_extension("jpeg"), None);
        assert_eq!(Jpeg::from_extension("JPG"), None);
        assert_eq!(Jpeg::from_extension("png"), None);
        assert_eq!(Gzip::from_extension("gz"), Some(Gzip));
        assert_eq!(Gzip::from_extension("gzip"), None);
    }

    #[test]
    fn matches_reports_the_raw_signature() {
        // A PNG that carries a PDF header matches both signatures; identify
        // still calls it a PNG.
        let mut head = png();
        head.extend(b"%PDF-1.7");
        assert!(Png::matches(&head));
        assert!(Pdf::matches(&head));
        assert!(!Jpeg::matches(&head));
    }
}
