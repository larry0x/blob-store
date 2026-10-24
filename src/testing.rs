use {
    crate::{Catalog, Format, Hasher, Inserted, Record},
    std::{
        cell::RefCell,
        collections::BTreeMap,
        sync::{
            Mutex, PoisonError,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, SystemTime},
    },
};

pub const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
pub const PDF_MAGIC: &[u8] = b"%PDF-";

crate::formats! {
    /// The formats the tests store: two presets.
    #[derive(PartialOrd, Ord)]
    pub enum TestFormat { Png, Pdf }
}

/// A PNG payload; a different `n` gives different bytes.
pub fn png(n: u8) -> Vec<u8> {
    let mut bytes = PNG_MAGIC.to_vec();
    bytes.extend([n; 100]);
    bytes
}

/// A PDF payload; a different `n` gives different bytes.
pub fn pdf(n: u8) -> Vec<u8> {
    let mut bytes = PDF_MAGIC.to_vec();
    bytes.extend([n; 100]);
    bytes
}

/// A fixed time, so the tests do not read the clock.
pub fn at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct MemError(pub &'static str);

/// A catalog in memory. A write works on a copy of the records, and the
/// commit swaps it in; the mutex stays held throughout, like a database
/// write lock.
pub struct Mem<O, F = TestFormat> {
    records: Mutex<BTreeMap<O, F>>,
    fail_commit: AtomicBool,
}

impl<O, F> Default for Mem<O, F> {
    fn default() -> Self {
        Self {
            records: Mutex::new(BTreeMap::new()),
            fail_commit: AtomicBool::new(false),
        }
    }
}

impl<O: Ord + Copy, F> Mem<O, F> {
    /// Make every later commit fail, or succeed again.
    pub fn fail_commits(&self, fail: bool) {
        self.fail_commit.store(fail, Ordering::SeqCst);
    }

    /// Write a record directly, as a raw write to the table would.
    pub fn insert_raw(&self, hash: O, format: F) {
        self.lock().insert(hash, format);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<O, F>> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<F: Format, H: Hasher> Catalog<F, H> for Mem<H::Output, F> {
    type Error = MemError;
    type Tx<'t> = RefCell<BTreeMap<H::Output, F>>;

    fn write<T, E: From<MemError>>(
        &self,
        f: impl FnOnce(&Self::Tx<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut records = self.lock();
        let tx = RefCell::new(records.clone());
        let value = f(&tx)?;
        if self.fail_commit.load(Ordering::SeqCst) {
            return Err(MemError("commit failed").into());
        }
        *records = tx.into_inner();
        Ok(value)
    }

    fn insert(tx: &Self::Tx<'_>, record: &Record<F, H>) -> Result<Inserted, MemError> {
        let mut records = tx.borrow_mut();
        if records.contains_key(&record.hash) {
            return Ok(Inserted::AlreadyPresent);
        }
        records.insert(record.hash, record.format);
        Ok(Inserted::New)
    }

    fn delete(tx: &Self::Tx<'_>, hash: &H::Output) -> Result<Option<F>, MemError> {
        Ok(tx.borrow_mut().remove(hash))
    }

    fn get_in(tx: &Self::Tx<'_>, hash: &H::Output) -> Result<Option<F>, MemError> {
        Ok(tx.borrow().get(hash).copied())
    }

    fn get(&self, hash: &H::Output) -> Result<Option<F>, MemError> {
        Ok(self.lock().get(hash).copied())
    }

    fn list(&self) -> Result<Vec<(H::Output, F)>, MemError> {
        Ok(self
            .lock()
            .iter()
            .map(|(hash, format)| (*hash, *format))
            .collect())
    }
}
