//! A content-addressed file store with a database catalog.
//!
//! A [`BlobStore`] names each file by the hash of its content, and keeps a
//! record of every file in a [`Catalog`]. If the catalog has a record, the
//! file exists. See `README.md` for the design and its guarantees.

/// The catalog trait, the database that records which blobs exist, and the
/// SQLite and redb catalogs behind their features.
mod catalog;
/// The error type of the store.
mod error;
/// The trait for the file formats a store accepts, and the presets and the
/// `formats!` macro behind the `format-presets` feature.
mod format;
/// The hasher trait, the digest type, and the BLAKE3 and SHA-256 hashers
/// behind their features.
mod hasher;
/// Streaming a file into `tmp/`, and the values that track it from there:
/// `Ingest`, `Staged`, and `Landed`.
mod ingest;
/// The state that a store shares with its blobs in progress: the file lock,
/// the hashes in flight, and the live temp files.
mod shared;
/// The store itself: opening, landing files, and catalog writes.
mod store;
/// Deleting the files that no record or save in progress accounts for.
mod sweep;
/// Helpers for the tests: a format enum of two presets, sample files, and an
/// in-memory catalog.
#[cfg(all(test, feature = "format-presets", feature = "hasher-blake3"))]
mod testing;
/// The transaction that a store lends to the app, and what a write returns.
mod tx;

pub use {catalog::*, error::*, format::*, hasher::*, ingest::*, store::*, sweep::*, tx::*};
