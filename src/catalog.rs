/// A catalog in redb.
///
/// [`crate::redb::Redb`] is ready to use. An app with its own database type
/// can implement [`crate::Catalog`] on it instead, by calling the free
/// functions here. Either way, the records live in the
/// [`crate::redb::BLOBS`] table.
#[cfg(feature = "catalog-redb")]
pub mod redb;
/// A catalog in SQLite, through `rusqlite`.
///
/// [`crate::sqlite::Sqlite`] is ready to use. An app with its own database
/// type can implement [`crate::Catalog`] on it instead, by calling the free
/// functions here. Either way, the records live in a table named `blob`:
///
/// ```sql
#[doc = include_str!("catalog/sqlite.sql")]
/// ```
#[cfg(feature = "catalog-sqlite")]
pub mod sqlite;

#[cfg(feature = "catalog-redb")]
pub use self::redb::Redb;
#[cfg(feature = "catalog-sqlite")]
pub use self::sqlite::Sqlite;
use {
    crate::{Format, Hasher},
    std::{error::Error as StdError, fmt, sync::Arc, time::SystemTime},
};

/// One blob, as the catalog records it.
pub struct Record<F, H: Hasher> {
    pub hash: H::Output,
    pub format: F,
    pub size_bytes: u64,
    pub added_at: SystemTime,
}

impl<F: fmt::Debug, H: Hasher> fmt::Debug for Record<F, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Record")
            .field("hash", &self.hash)
            .field("format", &self.format)
            .field("size_bytes", &self.size_bytes)
            .field("added_at", &self.added_at)
            .finish()
    }
}

/// What [`Catalog::insert`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inserted {
    New,
    /// A record for this hash was already there, and it is kept as it was.
    AlreadyPresent,
}

/// The database that records which blobs exist.
///
/// Only the store writes to it: through [`crate::BlobStore::write`], the
/// app adds its own rows to the same transaction.
pub trait Catalog<F: Format, H: Hasher> {
    /// A write transaction.
    type Tx<'t>;

    type Error: StdError + Send + Sync + 'static;

    /// Run `f` in one write transaction. Commit when `f` returns `Ok`, and
    /// roll back otherwise.
    fn write<T, E: From<Self::Error>>(
        &self,
        f: impl FnOnce(&Self::Tx<'_>) -> Result<T, E>,
    ) -> Result<T, E>;

    /// Record a blob. A record that is already there is not an error.
    fn insert(tx: &Self::Tx<'_>, record: &Record<F, H>) -> Result<Inserted, Self::Error>;

    /// Delete a record, and return its format. `None` if there was none.
    fn delete(tx: &Self::Tx<'_>, hash: &H::Output) -> Result<Option<F>, Self::Error>;

    /// The format of a record, read inside a write transaction.
    fn get_in(tx: &Self::Tx<'_>, hash: &H::Output) -> Result<Option<F>, Self::Error>;

    /// The format of a record, as last committed.
    fn get(&self, hash: &H::Output) -> Result<Option<F>, Self::Error>;

    /// Every record, as last committed, in hash order.
    fn list(&self) -> Result<Vec<(H::Output, F)>, Self::Error>;
}

/// An app can share one database handle between the store and its own code.
impl<F: Format, H: Hasher, C: Catalog<F, H>> Catalog<F, H> for Arc<C> {
    type Error = C::Error;
    type Tx<'t> = C::Tx<'t>;

    fn write<T, E: From<Self::Error>>(
        &self,
        f: impl FnOnce(&Self::Tx<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        C::write(self, f)
    }

    fn insert(tx: &Self::Tx<'_>, record: &Record<F, H>) -> Result<Inserted, Self::Error> {
        C::insert(tx, record)
    }

    fn delete(tx: &Self::Tx<'_>, hash: &H::Output) -> Result<Option<F>, Self::Error> {
        C::delete(tx, hash)
    }

    fn get_in(tx: &Self::Tx<'_>, hash: &H::Output) -> Result<Option<F>, Self::Error> {
        C::get_in(tx, hash)
    }

    fn get(&self, hash: &H::Output) -> Result<Option<F>, Self::Error> {
        C::get(self, hash)
    }

    fn list(&self) -> Result<Vec<(H::Output, F)>, Self::Error> {
        C::list(self)
    }
}

/// Nanoseconds since the Unix epoch, negative before it. `None` when the
/// time is out of the range of an `i64`: beyond about 292 years from 1970.
#[cfg(any(feature = "catalog-sqlite", feature = "catalog-redb"))]
pub(crate) fn nanoseconds(time: SystemTime) -> Option<i64> {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_nanos()).ok(),
        Err(before) => i64::try_from(before.duration().as_nanos()).ok().map(|n| -n),
    }
}
