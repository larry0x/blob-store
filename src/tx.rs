use {
    crate::{Catalog, Error, Format, Hasher, Inserted, Landed, Record, shared::Shared},
    std::{path::PathBuf, sync::Arc, time::SystemTime},
};

/// A catalog transaction, as [`crate::BlobStore::write`] lends it to the app.
pub struct BlobTx<'a, 't, C: Catalog<F, H>, F: Format, H: Hasher> {
    tx: &'a C::Tx<'t>,
    shared: &'a Arc<Shared<H::Output>>,
    landed: Vec<Landed<F, H>>,
    removed: Vec<(H::Output, F)>,
}

impl<'a, 't, C: Catalog<F, H>, F: Format, H: Hasher> BlobTx<'a, 't, C, F, H> {
    pub(crate) fn new(tx: &'a C::Tx<'t>, shared: &'a Arc<Shared<H::Output>>) -> Self {
        Self {
            tx,
            shared,
            landed: Vec::new(),
            removed: Vec::new(),
        }
    }

    pub(crate) fn into_parts(self) -> (Vec<Landed<F, H>>, Vec<(H::Output, F)>) {
        (self.landed, self.removed)
    }

    /// Record a landed blob. Its hash stays in flight until the transaction
    /// ends, so nothing deletes the file before the record commits.
    pub fn insert(
        &mut self,
        landed: Landed<F, H>,
        added_at: SystemTime,
    ) -> Result<Inserted, Error> {
        if !Arc::ptr_eq(&landed.shared, self.shared) {
            return Err(Error::WrongStore);
        }
        let record = Record {
            hash: landed.hash(),
            format: landed.format(),
            size_bytes: landed.size_bytes(),
            added_at,
        };
        let inserted = C::insert(self.tx, &record).map_err(Error::catalog)?;
        self.landed.push(landed);
        Ok(inserted)
    }

    /// Delete a record now, and its file after the commit. Returns the
    /// record's format, or `None` if there was no record.
    pub fn remove(&mut self, hash: &H::Output) -> Result<Option<F>, Error> {
        let format = C::delete(self.tx, hash).map_err(Error::catalog)?;
        if let Some(format) = format {
            self.removed.push((*hash, format));
        }
        Ok(format)
    }

    /// The format of a record, as this transaction sees it.
    pub fn get(&self, hash: &H::Output) -> Result<Option<F>, Error> {
        C::get_in(self.tx, hash).map_err(Error::catalog)
    }

    /// The raw transaction, for the app's own rows.
    pub fn inner(&self) -> &'a C::Tx<'t> {
        self.tx
    }
}

/// What [`crate::BlobStore::write`] returns on success.
#[derive(Debug)]
pub struct Written<T, O> {
    /// What the closure returned.
    pub value: T,
    /// Files whose records went, but which could not be deleted. Each stays
    /// until the next sweep.
    pub failed_removals: Vec<FailedRemoval<O>>,
}

impl<T, O> Written<T, O> {
    pub fn into_value(self) -> T {
        self.value
    }
}

#[derive(Debug)]
pub struct FailedRemoval<O> {
    pub hash: O,
    pub path: PathBuf,
    pub error: Error,
}
