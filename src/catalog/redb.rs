use {
    crate::{Catalog, Format, HashOutput, Hasher, Inserted, Record, catalog::nanoseconds},
    ::redb::{
        Database, ReadTransaction, ReadableDatabase, ReadableTable, TableDefinition, TableError,
        WriteTransaction,
    },
};

/// The records: the digest bytes, mapped to the extension of the format,
/// the size in bytes, and the time added in nanoseconds since the Unix epoch.
pub const BLOBS: TableDefinition<&[u8], (&str, u64, i64)> = TableDefinition::new("blobs");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Redb(#[from] ::redb::Error),
    #[error("corrupt blob record: {0}")]
    Corrupt(String),
    #[error("the {0} is out of the storable range")]
    OutOfRange(&'static str),
}

// redb reports each stage with its own error type; funnel them all into the
// one transparent variant.
macro_rules! from_redb_error {
    ($($error:ty),+ $(,)?) => {
        $(impl From<$error> for Error {
            fn from(error: $error) -> Self {
                Self::Redb(error.into())
            }
        })+
    };
}

from_redb_error!(
    ::redb::CommitError,
    ::redb::DatabaseError,
    ::redb::StorageError,
    ::redb::TableError,
    ::redb::TransactionError,
);

/// Record a blob. A record that is already there is kept as it was.
pub fn insert<F: Format, H: Hasher>(
    tx: &WriteTransaction,
    record: &Record<F, H>,
) -> Result<Inserted, Error> {
    let added_at = nanoseconds(record.added_at).ok_or(Error::OutOfRange("time"))?;
    let mut table = tx.open_table(BLOBS)?;
    let key = record.hash.as_ref();
    if table.get(key)?.is_some() {
        return Ok(Inserted::AlreadyPresent);
    }
    table.insert(
        key,
        (record.format.extension(), record.size_bytes, added_at),
    )?;
    Ok(Inserted::New)
}

/// Delete a record, and return its format.
pub fn delete<F: Format, H: Hasher>(
    tx: &WriteTransaction,
    hash: &H::Output,
) -> Result<Option<F>, Error> {
    let mut table = tx.open_table(BLOBS)?;
    let removed = table.remove(hash.as_ref())?;
    removed
        .map(|guard| parse_format(guard.value().0))
        .transpose()
}

/// The format of a record, inside a write transaction.
pub fn get_in<F: Format, H: Hasher>(
    tx: &WriteTransaction,
    hash: &H::Output,
) -> Result<Option<F>, Error> {
    let table = tx.open_table(BLOBS)?;
    let found = table.get(hash.as_ref())?;
    found.map(|guard| parse_format(guard.value().0)).transpose()
}

/// The format of a record.
pub fn get<F: Format, H: Hasher>(
    tx: &ReadTransaction,
    hash: &H::Output,
) -> Result<Option<F>, Error> {
    let table = match tx.open_table(BLOBS) {
        Ok(table) => table,
        Err(TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let found = table.get(hash.as_ref())?;
    found.map(|guard| parse_format(guard.value().0)).transpose()
}

/// Every record, in hash order.
pub fn list<F: Format, H: Hasher>(tx: &ReadTransaction) -> Result<Vec<(H::Output, F)>, Error> {
    let table = match tx.open_table(BLOBS) {
        Ok(table) => table,
        Err(TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut records = Vec::new();
    for entry in table.iter()? {
        let (key, value) = entry?;
        let hash = H::Output::from_slice(key.value())
            .ok_or_else(|| Error::Corrupt(format!("hash of {} bytes", key.value().len())))?;
        records.push((hash, parse_format(value.value().0)?));
    }
    Ok(records)
}

fn parse_format<F: Format>(extension: &str) -> Result<F, Error> {
    F::from_extension(extension).ok_or_else(|| Error::Corrupt(format!("format {extension:?}")))
}

/// A redb database used as a catalog. The app reads and writes its own
/// tables through [`Redb::database`].
pub struct Redb {
    db: Database,
}

impl Redb {
    /// Take over `db`, and create the [`BLOBS`] table if it is absent.
    pub fn new(db: Database) -> Result<Self, Error> {
        let tx = db.begin_write()?;
        tx.open_table(BLOBS)?;
        tx.commit()?;
        Ok(Self { db })
    }

    /// The database, for tables that hold no blob records.
    pub fn database(&self) -> &Database {
        &self.db
    }
}

impl<F: Format, H: Hasher> Catalog<F, H> for Redb {
    type Error = Error;
    type Tx<'t> = WriteTransaction;

    fn write<T, E: From<Error>>(
        &self,
        f: impl FnOnce(&WriteTransaction) -> Result<T, E>,
    ) -> Result<T, E> {
        let tx = self.db.begin_write().map_err(Error::from)?;
        // Dropping an uncommitted transaction aborts it.
        let value = f(&tx)?;
        tx.commit().map_err(Error::from)?;
        Ok(value)
    }

    fn insert(tx: &WriteTransaction, record: &Record<F, H>) -> Result<Inserted, Error> {
        insert(tx, record)
    }

    fn delete(tx: &WriteTransaction, hash: &H::Output) -> Result<Option<F>, Error> {
        delete::<F, H>(tx, hash)
    }

    fn get_in(tx: &WriteTransaction, hash: &H::Output) -> Result<Option<F>, Error> {
        get_in::<F, H>(tx, hash)
    }

    fn get(&self, hash: &H::Output) -> Result<Option<F>, Error> {
        get::<F, H>(&self.db.begin_read()?, hash)
    }

    fn list(&self) -> Result<Vec<(H::Output, F)>, Error> {
        list::<F, H>(&self.db.begin_read()?)
    }
}

#[cfg(all(test, feature = "format-presets", feature = "hasher-blake3"))]
mod tests {
    use {
        super::*,
        crate::{
            BlobStore, Digest, Error as StoreError, Written,
            testing::{TestFormat, at, pdf, png},
        },
        tempfile::TempDir,
    };

    type Store = BlobStore<Redb, TestFormat>;
    type Hash = Digest<32>;

    /// An app table that lives beside the blob records.
    const NOTES: TableDefinition<&[u8], &str> = TableDefinition::new("notes");

    fn open() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let db = Database::create(dir.path().join("db.redb")).unwrap();
        let store = Store::open(dir.path().join("blobs"), Redb::new(db).unwrap()).unwrap();
        (dir, store)
    }

    fn add(store: &Store, bytes: &[u8]) -> Hash {
        let landed = store.land(store.save(bytes).unwrap()).unwrap();
        let hash = landed.hash();
        store.write(|tx| tx.insert(landed, at())).unwrap();
        hash
    }

    fn note(store: &Store, hash: &Hash) -> Option<String> {
        let tx = store.catalog().database().begin_read().unwrap();
        let table = match tx.open_table(NOTES) {
            Ok(table) => table,
            Err(TableError::TableDoesNotExist(_)) => return None,
            Err(error) => panic!("{error}"),
        };
        table
            .get(hash.as_ref())
            .unwrap()
            .map(|guard| guard.value().to_owned())
    }

    fn db(error: impl Into<::redb::Error>) -> StoreError {
        StoreError::catalog(error.into())
    }

    #[test]
    fn records_round_trip() {
        let (_dir, store) = open();
        let a = add(&store, &png(1));
        let b = add(&store, &pdf(2));
        let mut expected = vec![(a, TestFormat::Png), (b, TestFormat::Pdf)];
        expected.sort();
        assert_eq!(store.list().unwrap(), expected);
        assert_eq!(store.get(&b).unwrap(), Some(TestFormat::Pdf));

        let written = store
            .write(|tx| {
                assert_eq!(tx.get(&a)?, Some(TestFormat::Png));
                tx.remove(&a)
            })
            .unwrap();
        assert_eq!(written.value, Some(TestFormat::Png));
        assert_eq!(store.get(&a).unwrap(), None);
        assert!(!store.path_of(&a, TestFormat::Png).exists());
    }

    #[test]
    fn a_second_insert_keeps_the_first_record() {
        let (_dir, store) = open();
        add(&store, &png(3));
        let landed = store.land(store.save(&png(3)).unwrap()).unwrap();
        let written = store.write(|tx| tx.insert(landed, at())).unwrap();
        assert_eq!(written.value, Inserted::AlreadyPresent);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn app_rows_commit_with_the_blob_record() {
        let (_dir, store) = open();
        let landed = store.land(store.save(&png(4)).unwrap()).unwrap();
        let hash = landed.hash();
        store
            .write(|tx| {
                tx.insert(landed, at())?;
                let mut notes = tx.inner().open_table(NOTES).map_err(db)?;
                notes.insert(hash.as_ref(), "hello").map_err(db)?;
                Ok::<_, StoreError>(())
            })
            .unwrap();
        assert_eq!(store.get(&hash).unwrap(), Some(TestFormat::Png));
        assert_eq!(note(&store, &hash).as_deref(), Some("hello"));
    }

    #[test]
    fn a_failure_rolls_back_the_app_rows_and_the_blob_record() {
        let (_dir, store) = open();
        let landed = store.land(store.save(&png(5)).unwrap()).unwrap();
        let hash = landed.hash();
        let result: Result<Written<(), Hash>, StoreError> = store.write(|tx| {
            tx.insert(landed, at())?;
            let mut notes = tx.inner().open_table(NOTES).map_err(db)?;
            notes.insert(hash.as_ref(), "hello").map_err(db)?;
            Err(StoreError::UnknownFormat)
        });
        assert!(result.is_err());
        assert_eq!(store.get(&hash).unwrap(), None);
        assert_eq!(note(&store, &hash), None);
        assert_eq!(store.sweep().unwrap().removed_orphans.len(), 1);
    }

    #[test]
    fn corrupt_records_are_reported() {
        let (_dir, store) = open();
        let tx = store.catalog().database().begin_write().unwrap();
        tx.open_table(BLOBS)
            .unwrap()
            .insert(&[7u8; 32][..], ("exe", 1, 0))
            .unwrap();
        tx.open_table(BLOBS)
            .unwrap()
            .insert(&[8u8; 3][..], ("png", 1, 0))
            .unwrap();
        tx.commit().unwrap();
        assert!(matches!(
            store.get(&Hash::from_bytes([7; 32])),
            Err(StoreError::Catalog(_))
        ));
        assert!(matches!(store.list(), Err(StoreError::Catalog(_))));
    }

    #[test]
    fn a_database_without_the_table_reads_as_empty() {
        let dir = TempDir::new().unwrap();
        let db = Database::create(dir.path().join("db.redb")).unwrap();
        let tx = db.begin_read().unwrap();
        assert_eq!(list::<TestFormat, crate::Blake3>(&tx).unwrap(), []);
        assert_eq!(
            get::<TestFormat, crate::Blake3>(&tx, &Hash::from_bytes([1; 32])).unwrap(),
            None
        );
    }
}
