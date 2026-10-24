use {
    crate::{Catalog, Format, Hasher, Inserted, Record, catalog::nanoseconds},
    rusqlite::{Connection, OptionalExtension, Transaction, params},
    std::sync::{Mutex, MutexGuard, PoisonError},
};

/// The `blob` table, created only if absent.
pub const SCHEMA: &str = include_str!("sqlite.sql");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("corrupt blob record: {0}")]
    Corrupt(String),
    #[error("the {0} is out of the storable range")]
    OutOfRange(&'static str),
}

/// Record a blob. A record that is already there is kept as it was.
pub fn insert<F: Format, H: Hasher>(
    conn: &Connection,
    record: &Record<F, H>,
) -> Result<Inserted, Error> {
    let size_bytes = i64::try_from(record.size_bytes).map_err(|_| Error::OutOfRange("size"))?;
    let added_at = nanoseconds(record.added_at).ok_or(Error::OutOfRange("time"))?;
    let changed = conn
        .prepare_cached(
            "INSERT INTO blob (hash, format, size_bytes, added_at) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (hash) DO NOTHING",
        )?
        .execute(params![
            record.hash.to_string(),
            record.format.extension(),
            size_bytes,
            added_at,
        ])?;
    Ok(match changed {
        0 => Inserted::AlreadyPresent,
        _ => Inserted::New,
    })
}

/// Delete a record, and return its format.
pub fn delete<F: Format, H: Hasher>(
    conn: &Connection,
    hash: &H::Output,
) -> Result<Option<F>, Error> {
    let format = conn
        .prepare_cached("DELETE FROM blob WHERE hash = ?1 RETURNING format")?
        .query_row([hash.to_string()], |row| row.get::<_, String>(0))
        .optional()?;
    format.map(|format| parse_format(&format)).transpose()
}

/// The format of a record.
pub fn get<F: Format, H: Hasher>(conn: &Connection, hash: &H::Output) -> Result<Option<F>, Error> {
    let format = conn
        .prepare_cached("SELECT format FROM blob WHERE hash = ?1")?
        .query_row([hash.to_string()], |row| row.get::<_, String>(0))
        .optional()?;
    format.map(|format| parse_format(&format)).transpose()
}

/// Every record, in hash order.
pub fn list<F: Format, H: Hasher>(conn: &Connection) -> Result<Vec<(H::Output, F)>, Error> {
    let mut statement = conn.prepare_cached("SELECT hash, format FROM blob ORDER BY hash")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| {
        let (hash, format) = row?;
        let hash = hash
            .parse::<H::Output>()
            .map_err(|error| Error::Corrupt(format!("hash {hash:?}: {error}")))?;
        Ok((hash, parse_format(&format)?))
    })
    .collect()
}

fn parse_format<F: Format>(extension: &str) -> Result<F, Error> {
    F::from_extension(extension).ok_or_else(|| Error::Corrupt(format!("format {extension:?}")))
}

/// A SQLite connection behind a mutex, used as a catalog. The app reads and
/// writes its own tables through [`Sqlite::read`] and [`Sqlite::write`].
pub struct Sqlite {
    conn: Mutex<Connection>,
}

impl Sqlite {
    /// Take over `conn`, and create the `blob` table if it is absent.
    pub fn new(conn: Connection) -> Result<Self, Error> {
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic inside a transaction rolls it back as the guard unwinds,
        // so a poisoned connection is still consistent.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run `f` against the connection, outside any write transaction.
    pub fn read<T, E>(&self, f: impl FnOnce(&Connection) -> Result<T, E>) -> Result<T, E> {
        f(&self.lock())
    }

    /// Run `f` in one write transaction, for rows that touch no blob.
    /// Commit when it returns `Ok`, and roll back otherwise.
    pub fn write<T, E: From<rusqlite::Error>>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }
}

impl<F: Format, H: Hasher> Catalog<F, H> for Sqlite {
    type Error = Error;
    type Tx<'t> = Transaction<'t>;

    fn write<T, E: From<Error>>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(Error::from)?;
        let value = f(&tx)?;
        tx.commit().map_err(Error::from)?;
        Ok(value)
    }

    fn insert(tx: &Transaction<'_>, record: &Record<F, H>) -> Result<Inserted, Error> {
        insert(tx, record)
    }

    fn delete(tx: &Transaction<'_>, hash: &H::Output) -> Result<Option<F>, Error> {
        delete::<F, H>(tx, hash)
    }

    fn get_in(tx: &Transaction<'_>, hash: &H::Output) -> Result<Option<F>, Error> {
        get::<F, H>(tx, hash)
    }

    fn get(&self, hash: &H::Output) -> Result<Option<F>, Error> {
        get::<F, H>(&self.lock(), hash)
    }

    fn list(&self) -> Result<Vec<(H::Output, F)>, Error> {
        list::<F, H>(&self.lock())
    }
}

#[cfg(all(test, feature = "format-presets", feature = "hasher-blake3"))]
mod tests {
    use {
        super::*,
        crate::{
            Blake3, BlobStore, Digest, Error as StoreError, Written,
            testing::{TestFormat, at, pdf, png},
        },
        std::time::{Duration, SystemTime},
        tempfile::TempDir,
    };

    type Store = BlobStore<Sqlite, TestFormat>;
    type Hash = Digest<32>;

    /// A store whose database also holds an app table that refers to blobs.
    fn open() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let conn = Connection::open(dir.path().join("db.sqlite")).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        let catalog = Sqlite::new(conn).unwrap();
        catalog
            .write(|tx| {
                tx.execute_batch("CREATE TABLE note (hash TEXT NOT NULL REFERENCES blob (hash))")
            })
            .unwrap();
        let store = Store::open(dir.path().join("blobs"), catalog).unwrap();
        (dir, store)
    }

    fn add(store: &Store, bytes: &[u8]) -> Hash {
        let landed = store.land(store.save(bytes).unwrap()).unwrap();
        let hash = landed.hash();
        store.write(|tx| tx.insert(landed, at())).unwrap();
        hash
    }

    fn notes(store: &Store) -> i64 {
        store
            .catalog()
            .read(|conn| conn.query_row("SELECT count(*) FROM note", [], |row| row.get(0)))
            .unwrap()
    }

    fn sql(error: rusqlite::Error) -> StoreError {
        StoreError::catalog(error)
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

        let written = store.write(|tx| tx.remove(&a)).unwrap();
        assert_eq!(written.value, Some(TestFormat::Png));
        assert_eq!(store.get(&a).unwrap(), None);
        assert!(!store.path_of(&a, TestFormat::Png).exists());
        assert_eq!(store.write(|tx| tx.remove(&a)).unwrap().value, None);
    }

    #[test]
    fn a_second_insert_keeps_the_first_record() {
        let (_dir, store) = open();
        let hash = add(&store, &png(3));
        let landed = store.land(store.save(&png(3)).unwrap()).unwrap();
        let later = at() + Duration::from_secs(60);
        let written = store.write(|tx| tx.insert(landed, later)).unwrap();
        assert_eq!(written.value, Inserted::AlreadyPresent);
        let added_at: i64 = store
            .catalog()
            .read(|conn| {
                conn.query_row(
                    "SELECT added_at FROM blob WHERE hash = ?1",
                    [hash.to_string()],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(added_at, 1_800_000_000 * 1_000_000_000);
    }

    #[test]
    fn app_rows_commit_with_the_blob_record() {
        let (_dir, store) = open();
        let landed = store.land(store.save(&png(4)).unwrap()).unwrap();
        let hash = landed.hash();
        store
            .write(|tx| {
                tx.insert(landed, at())?;
                tx.inner()
                    .execute("INSERT INTO note (hash) VALUES (?1)", [hash.to_string()])
                    .map_err(sql)
            })
            .unwrap();
        assert_eq!(store.get(&hash).unwrap(), Some(TestFormat::Png));
        assert_eq!(notes(&store), 1);
    }

    #[test]
    fn a_failure_rolls_back_the_app_rows_and_the_blob_record() {
        let (_dir, store) = open();
        let landed = store.land(store.save(&png(5)).unwrap()).unwrap();
        let hash = landed.hash();
        let result: Result<Written<(), Hash>, StoreError> = store.write(|tx| {
            tx.insert(landed, at())?;
            tx.inner()
                .execute("INSERT INTO note (hash) VALUES (?1)", [hash.to_string()])
                .map_err(sql)?;
            Err(StoreError::UnknownFormat)
        });
        assert!(result.is_err());
        assert_eq!(store.get(&hash).unwrap(), None);
        assert_eq!(notes(&store), 0);
        assert_eq!(store.sweep().unwrap().removed_orphans.len(), 1);
    }

    #[test]
    fn a_foreign_key_blocks_a_note_without_a_blob() {
        let (_dir, store) = open();
        let hash = Blake3::digest(b"never saved");
        let result = store.write(|tx| {
            tx.inner()
                .execute("INSERT INTO note (hash) VALUES (?1)", [hash.to_string()])
                .map_err(sql)
        });
        assert!(result.is_err());
    }

    #[test]
    fn corrupt_records_are_reported() {
        let (_dir, store) = open();
        let hash = Blake3::digest(b"corrupt");
        store
            .catalog()
            .write(|tx| {
                tx.execute(
                    "INSERT INTO blob (hash, format, size_bytes, added_at) VALUES (?1, 'exe', 1, 0)",
                    [hash.to_string()],
                )
            })
            .unwrap();
        assert!(matches!(store.get(&hash), Err(StoreError::Catalog(_))));
        assert!(matches!(store.list(), Err(StoreError::Catalog(_))));
    }

    #[test]
    fn the_table_rejects_malformed_hashes() {
        let (_dir, store) = open();
        for bad in ["AB".repeat(32), "zz".repeat(32), String::new()] {
            let result = store.catalog().write(|tx| {
                tx.execute(
                    "INSERT INTO blob (hash, format, size_bytes, added_at) VALUES (?1, 'png', 1, 0)",
                    [&bad],
                )
            });
            assert!(result.is_err(), "{bad:?}");
        }
    }

    #[test]
    fn times_store_as_signed_nanoseconds() {
        let (_dir, store) = open();
        let before_epoch = SystemTime::UNIX_EPOCH - Duration::from_secs(1);
        let landed = store.land(store.save(&png(6)).unwrap()).unwrap();
        let hash = landed.hash();
        store.write(|tx| tx.insert(landed, before_epoch)).unwrap();
        let added_at: i64 = store
            .catalog()
            .read(|conn| {
                conn.query_row(
                    "SELECT added_at FROM blob WHERE hash = ?1",
                    [hash.to_string()],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(added_at, -1_000_000_000);

        // About 317 years past 1970: beyond an i64 of nanoseconds.
        let far = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000_000);
        let landed = store.land(store.save(&png(7)).unwrap()).unwrap();
        let hash = landed.hash();
        assert!(store.write(|tx| tx.insert(landed, far)).is_err());
        assert_eq!(store.get(&hash).unwrap(), None);
    }
}
