/// Define a format enum whose variants are presets.
///
/// Each variant must be named after a preset: `Jpeg`, `Png`, `Gif`, `Webp`,
/// `Webm`, `Mp4`, `Mov`, `Gzip`, or `Pdf`. The enum gets:
///
/// - the derives that [`crate::Format`] needs: `Clone`, `Copy`, `Debug`,
///   `PartialEq`, `Eq`, and `Hash`. Do not derive them again.
/// - an `impl Format`. Sniffing first asks [`crate::identify`] which preset
///   the file is, then accepts it only if that preset is a variant. A file of
///   another format is rejected, whatever the order of the variants.
/// - `ALL`, every variant in the order written, and `media_type()`.
///
/// Attributes on the enum and on its variants pass through, so other derives
/// and attributes such as `#[serde(rename = "…")]` work.
///
/// The one exception is `#[cfg]`, on the enum or on a variant, also when a
/// `#[cfg_attr]` would add one. The generated code names every variant, so a
/// variant that `cfg` removed would break the build with a confusing error.
/// The macro stops with a clear one instead. To vary the formats by
/// configuration, write one `formats!` per configuration.
///
/// ```
/// blob_store::formats! {
///     #[derive(PartialOrd, Ord)]
///     pub enum Media { Jpeg, Png, Mp4, Mov }
/// }
///
/// use blob_store::Format;
/// assert_eq!(Media::Mov.extension(), "mov");
/// assert_eq!(Media::from_extension("jpg"), Some(Media::Jpeg));
/// assert_eq!(Media::Jpeg.media_type(), "image/jpeg");
/// assert_eq!(Media::sniff(b"%PDF-1.7"), None);
/// ```
///
/// A `cfg_attr` that adds other attributes is fine:
///
/// ```
/// blob_store::formats! {
///     #[cfg_attr(all(), derive(PartialOrd, Ord))]
///     pub enum Media {
///         /// Not a cfg(...) attribute, only text that mentions one.
///         Jpeg,
///         Png,
///     }
/// }
/// assert!(Media::Jpeg < Media::Png);
/// ```
///
/// Each of these stops the build, even though the condition is true:
///
/// ```compile_fail
/// blob_store::formats! {
///     pub enum Media { Jpeg, #[cfg(all())] Png }
/// }
/// ```
///
/// ```compile_fail
/// blob_store::formats! {
///     #[cfg(all())]
///     pub enum Media { Jpeg, Png }
/// }
/// ```
///
/// ```compile_fail
/// blob_store::formats! {
///     pub enum Media { Jpeg, #[cfg_attr(all(), cfg(all()))] Png }
/// }
/// ```
#[macro_export]
macro_rules! formats {
    (
        $(#[$($attr:tt)*])*
        $vis:vis enum $name:ident {
            $($(#[$($variant_attr:tt)*])* $variant:ident),+ $(,)?
        }
    ) => {
        $($crate::__formats_reject_cfg!($($attr)*);)*
        $($($crate::__formats_reject_cfg!($($variant_attr)*);)*)+

        $(#[$($attr)*])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        $vis enum $name {
            $($(#[$($variant_attr)*])* $variant,)+
        }

        // An app may use neither item.
        #[allow(dead_code)]
        impl $name {
            /// Every format, in the order written.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The media type, as an HTTP response or a data URI names it.
            pub fn media_type(&self) -> &'static str {
                match self {
                    $(Self::$variant => <$crate::$variant as $crate::Preset>::MEDIA_TYPE,)+
                }
            }
        }

        impl $crate::Format for $name {
            fn sniff(head: &[u8]) -> ::core::option::Option<Self> {
                <Self as $crate::Format>::from_extension($crate::identify(head)?)
            }

            fn extension(&self) -> &'static str {
                match self {
                    $(Self::$variant => <$crate::$variant as $crate::Preset>::EXTENSION,)+
                }
            }

            fn from_extension(extension: &str) -> ::core::option::Option<Self> {
                $(
                    if extension == <$crate::$variant as $crate::Preset>::EXTENSION {
                        return ::core::option::Option::Some(Self::$variant);
                    }
                )+
                ::core::option::Option::None
            }
        }
    };
}

/// Stop the build if the tokens of an attribute hold `cfg(…)` at any depth:
/// a bare `#[cfg(…)]`, or one that a `#[cfg_attr(…)]` would add.
#[doc(hidden)]
#[macro_export]
macro_rules! __formats_reject_cfg {
    () => {};
    (cfg ($($_condition:tt)*) $($_rest:tt)*) => {
        ::core::compile_error!(
            "`formats!` does not support `#[cfg]`, also not through `#[cfg_attr]`; \
             write one `formats!` per configuration instead"
        );
    };
    (($($inner:tt)*) $($rest:tt)*) => {
        $crate::__formats_reject_cfg!($($inner)* $($rest)*);
    };
    ([$($inner:tt)*] $($rest:tt)*) => {
        $crate::__formats_reject_cfg!($($inner)* $($rest)*);
    };
    ({$($inner:tt)*} $($rest:tt)*) => {
        $crate::__formats_reject_cfg!($($inner)* $($rest)*);
    };
    ($_first:tt $($rest:tt)*) => {
        $crate::__formats_reject_cfg!($($rest)*);
    };
}

#[cfg(test)]
mod tests {
    use crate::{Format, Preset};

    crate::formats! {
        /// What an image board takes.
        #[derive(PartialOrd, Ord)]
        enum Media { Jpeg, Png, Mp4, Mov }
    }

    crate::formats! {
        enum Documents { Pdf, Gzip, }
    }

    fn png() -> Vec<u8> {
        vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D,
        ]
    }

    fn ftyp(brand: &[u8; 4]) -> Vec<u8> {
        let mut bytes = vec![0x00, 0x00, 0x00, 0x14];
        bytes.extend(b"ftyp");
        bytes.extend(brand);
        bytes.extend([0x00, 0x00, 0x02, 0x00]);
        bytes.extend(brand);
        bytes
    }

    #[test]
    fn members_are_sniffed_as_themselves() {
        assert_eq!(Media::sniff(&png()), Some(Media::Png));
        assert_eq!(Media::sniff(&ftyp(b"isom")), Some(Media::Mp4));
        assert_eq!(Media::sniff(&ftyp(b"qt  ")), Some(Media::Mov));
        assert_eq!(Documents::sniff(b"%PDF-1.7\n"), Some(Documents::Pdf));
    }

    #[test]
    fn other_formats_are_rejected() {
        assert_eq!(Media::sniff(b"%PDF-1.7\n"), None);
        assert_eq!(Media::sniff(b"GIF89a\x01\x00\x01\x00"), None);
        assert_eq!(Media::sniff(b"just some text"), None);
        assert_eq!(Documents::sniff(&png()), None);
    }

    #[test]
    fn a_member_is_not_claimed_for_another_presets_file() {
        // The PNG carries a PDF header. Documents takes PDFs, but the file
        // is a PNG, so it is rejected rather than read as a PDF.
        let mut head = png();
        head.extend(b"%PDF-1.7");
        assert_eq!(Documents::sniff(&head), None);
        assert_eq!(Media::sniff(&head), Some(Media::Png));
    }

    #[test]
    fn extensions_round_trip_for_members_only() {
        for format in Media::ALL {
            assert_eq!(Media::from_extension(format.extension()), Some(*format));
        }
        assert_eq!(Media::Jpeg.extension(), "jpg");
        assert_eq!(Media::from_extension("pdf"), None);
        assert_eq!(Media::from_extension("jpeg"), None);
        assert_eq!(Documents::Gzip.extension(), "gz");
    }

    #[test]
    fn all_lists_the_variants_in_the_order_written() {
        assert_eq!(
            Media::ALL,
            [Media::Jpeg, Media::Png, Media::Mp4, Media::Mov]
        );
        assert_eq!(Documents::ALL, [Documents::Pdf, Documents::Gzip]);
    }

    #[test]
    fn media_types_come_from_the_presets() {
        assert_eq!(Media::Mov.media_type(), crate::Mov::MEDIA_TYPE);
        assert_eq!(Media::Png.media_type(), "image/png");
        assert_eq!(Documents::Pdf.media_type(), "application/pdf");
    }

    #[test]
    fn attributes_pass_through() {
        // `Ord` came from the attribute on the enum.
        assert!(Media::Jpeg < Media::Mov);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn variant_attributes_pass_through() {
        crate::formats! {
            #[derive(serde::Serialize, serde::Deserialize)]
            #[serde(rename_all = "lowercase")]
            enum Wire {
                #[serde(rename = "jpg")]
                Jpeg,
                Png,
            }
        }
        assert_eq!(serde_json::to_string(&Wire::Jpeg).unwrap(), "\"jpg\"");
        assert_eq!(serde_json::to_string(&Wire::Png).unwrap(), "\"png\"");
        assert_eq!(serde_json::from_str::<Wire>("\"jpg\"").unwrap(), Wire::Jpeg);
    }

    #[cfg(feature = "hasher-blake3")]
    #[test]
    fn a_store_takes_a_macro_enum() {
        use crate::{
            BlobStore, Digest,
            testing::{Mem, at},
        };

        let dir = tempfile::TempDir::new().unwrap();
        let store =
            BlobStore::<Mem<Digest<32>, Media>, Media>::open(dir.path(), Mem::default()).unwrap();
        let landed = store.land(store.save(&ftyp(b"qt  ")).unwrap()).unwrap();
        let hash = landed.hash();
        assert!(landed.path().to_str().unwrap().ends_with(".mov"));
        store.write(|tx| tx.insert(landed, at())).unwrap();
        assert_eq!(store.get(&hash).unwrap(), Some(Media::Mov));

        let staged = store.save(b"%PDF-1.7\n").unwrap();
        assert_eq!(staged.format(), None);
    }
}
