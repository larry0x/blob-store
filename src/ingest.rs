use {
    crate::{Error, Format, Hasher, shared::Shared},
    std::{
        ffi::OsString,
        fmt,
        io::Write,
        marker::PhantomData,
        path::{Path, PathBuf},
        sync::Arc,
    },
    tempfile::NamedTempFile,
};

/// How many leading bytes [`Format::sniff`] gets to see.
pub const SNIFF_LEN: usize = 8192;

/// A file in `tmp/`, registered as live so that a sweep leaves it alone.
/// Dropping it deletes the file.
pub(crate) struct Temp<O: Eq + std::hash::Hash + Copy> {
    file: Option<NamedTempFile>,
    name: OsString,
    pub shared: Arc<Shared<O>>,
}

impl<O: Eq + std::hash::Hash + Copy> Temp<O> {
    /// Call with the file lock held, so that a sweep never sees the file
    /// before it is registered.
    pub fn create(shared: Arc<Shared<O>>, dir: &Path) -> Result<Self, Error> {
        let file = tempfile::Builder::new()
            .prefix("ingest-")
            .tempfile_in(dir)?;
        let name = file
            .path()
            .file_name()
            .expect("a temp file has a name")
            .to_owned();
        shared.register_temp(name.clone());
        Ok(Self {
            file: Some(file),
            name,
            shared,
        })
    }

    fn file(&mut self) -> &mut NamedTempFile {
        self.file.as_mut().expect("the file stays until persisted")
    }

    /// Rename the file to `path`. On failure, the file is deleted.
    pub fn persist(mut self, path: &Path) -> Result<(), Error> {
        let file = self.file.take().expect("the file stays until persisted");
        file.persist(path)?;
        Ok(())
    }
}

impl<O: Eq + std::hash::Hash + Copy> Drop for Temp<O> {
    fn drop(&mut self) {
        // Delete first, then unregister: a sweep in between finds a live
        // name and skips it.
        drop(self.file.take());
        self.shared.unregister_temp(&self.name);
    }
}

/// A file being streamed into `tmp/`, hashed as it arrives.
///
/// Dropping it deletes the temp file.
pub struct Ingest<F, H: Hasher> {
    temp: Temp<H::Output>,
    hasher: H,
    size_bytes: u64,
    head: Vec<u8>,
    _format: PhantomData<fn() -> F>,
}

impl<F: Format, H: Hasher> Ingest<F, H> {
    pub(crate) fn new(temp: Temp<H::Output>) -> Self {
        Self {
            temp,
            hasher: H::default(),
            size_bytes: 0,
            head: Vec::new(),
            _format: PhantomData,
        }
    }

    pub fn write(&mut self, chunk: &[u8]) -> Result<(), Error> {
        self.temp.file().write_all(chunk)?;
        self.hasher.update(chunk);
        self.size_bytes += chunk.len() as u64;
        if self.head.len() < SNIFF_LEN {
            let take = (SNIFF_LEN - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..take]);
        }
        Ok(())
    }

    /// Flush and fsync the file, then derive its hash and format.
    pub fn finish(mut self) -> Result<Staged<F, H>, Error> {
        let file = self.temp.file();
        file.flush()?;
        file.as_file().sync_all()?;
        Ok(Staged {
            hash: self.hasher.finalize(),
            size_bytes: self.size_bytes,
            format: F::sniff(&self.head),
            temp: self.temp,
        })
    }
}

impl<F, H: Hasher> fmt::Debug for Ingest<F, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ingest")
            .field("size_bytes", &self.size_bytes)
            .finish_non_exhaustive()
    }
}

/// A fully received file, still in `tmp/`. Land it, or drop it to delete the
/// temp file.
pub struct Staged<F, H: Hasher> {
    pub(crate) temp: Temp<H::Output>,
    hash: H::Output,
    size_bytes: u64,
    format: Option<F>,
}

impl<F: Format, H: Hasher> Staged<F, H> {
    pub fn hash(&self) -> H::Output {
        self.hash
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// The sniffed format. `None` means the file can't be landed.
    pub fn format(&self) -> Option<F> {
        self.format
    }
}

impl<F: fmt::Debug, H: Hasher> fmt::Debug for Staged<F, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Staged")
            .field("hash", &self.hash)
            .field("size_bytes", &self.size_bytes)
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

/// The proof that a file is in place. Only [`crate::BlobStore::land`] makes
/// one, and only [`crate::BlobTx::insert`] can record it.
///
/// Its hash stays in flight until it is dropped: a sweep or a delete leaves
/// the file alone until then. Dropping it without recording it leaves an
/// orphan file, which the next sweep removes.
pub struct Landed<F, H: Hasher> {
    pub(crate) shared: Arc<Shared<H::Output>>,
    hash: H::Output,
    format: F,
    size_bytes: u64,
    path: PathBuf,
}

impl<F: Format, H: Hasher> Landed<F, H> {
    /// Call with the file lock held. Marks the hash in flight.
    pub(crate) fn new(
        shared: Arc<Shared<H::Output>>,
        hash: H::Output,
        format: F,
        size_bytes: u64,
        path: PathBuf,
    ) -> Self {
        shared.mark(hash);
        Self {
            shared,
            hash,
            format,
            size_bytes,
            path,
        }
    }

    pub fn hash(&self) -> H::Output {
        self.hash
    }

    pub fn format(&self) -> F {
        self.format
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl<F, H: Hasher> Drop for Landed<F, H> {
    fn drop(&mut self) {
        // Only the state lock: this runs inside write closures, where the
        // catalog is held.
        self.shared.unmark(&self.hash);
    }
}

impl<F: fmt::Debug, H: Hasher> fmt::Debug for Landed<F, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Landed")
            .field("hash", &self.hash)
            .field("format", &self.format)
            .field("size_bytes", &self.size_bytes)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}
