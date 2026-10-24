use {
    crate::{
        BlobTx, Catalog, Error, FailedRemoval, Format, HashOutput, Hasher, Ingest, Landed, Staged,
        Written, ingest::Temp, shared::Shared,
    },
    std::{
        fs::{self, File, TryLockError},
        io,
        marker::PhantomData,
        path::{Path, PathBuf},
        sync::Arc,
    },
};

pub(crate) const TMP_DIR: &str = "tmp";
pub(crate) const LAYOUT_FILE: &str = "layout.toml";
pub(crate) const LOCK_FILE: &str = "lock";

/// A content-addressed file tree, kept in sync with a catalog.
///
/// - `C` is the catalog.
/// - `F` is the set of file formats the store accepts.
/// - `H` is the hash function that names the files.
/// - `N` is the number of directory levels, one per leading byte of the hash.
///
/// With the `hasher-blake3` feature, `H` defaults to BLAKE3. See `README.md` for
/// the guarantees.
#[cfg(feature = "hasher-blake3")]
pub struct BlobStore<C, F, H: Hasher = crate::Blake3, const N: usize = 2> {
    catalog: C,
    shared: Arc<Shared<H::Output>>,
    _format: PhantomData<fn() -> F>,
}

/// A content-addressed file tree, kept in sync with a catalog.
///
/// - `C` is the catalog.
/// - `F` is the set of file formats the store accepts.
/// - `H` is the hash function that names the files.
/// - `N` is the number of directory levels, one per leading byte of the hash.
///
/// With the `hasher-blake3` feature, `H` defaults to BLAKE3. See `README.md` for
/// the guarantees.
#[cfg(not(feature = "hasher-blake3"))]
pub struct BlobStore<C, F, H: Hasher, const N: usize = 2> {
    catalog: C,
    shared: Arc<Shared<H::Output>>,
    _format: PhantomData<fn() -> F>,
}

#[cfg(feature = "hasher-blake3")]
impl<C: Catalog<F, crate::Blake3>, F: Format> BlobStore<C, F> {
    /// Open the store at `root` with BLAKE3 and two directory levels.
    pub fn open(root: impl Into<PathBuf>, catalog: C) -> Result<Self, Error> {
        Self::open_with(root, catalog)
    }
}

impl<C: Catalog<F, H>, F: Format, H: Hasher, const N: usize> BlobStore<C, F, H, N> {
    /// Open the store at `root`, creating it if needed.
    ///
    /// Fails if another process has the store open, or if the store was
    /// created with another hasher or another number of levels.
    pub fn open_with(root: impl Into<PathBuf>, catalog: C) -> Result<Self, Error> {
        const { assert!(N <= H::LEN, "a hash has fewer bytes than directory levels") };

        let root = root.into();
        create_root(&root)?;

        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join(LOCK_FILE))?;
        match lock.try_lock() {
            Ok(()) => {},
            Err(TryLockError::WouldBlock) => return Err(Error::Locked(root)),
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }

        check_layout(&root, &format!("hasher = \"{}\"\ndepth  = {N}\n", H::NAME))?;

        Ok(Self {
            catalog,
            shared: Arc::new(Shared::new(root, lock)),
            _format: PhantomData,
        })
    }

    pub fn root(&self) -> &Path {
        &self.shared.root
    }

    pub fn catalog(&self) -> &C {
        &self.catalog
    }

    /// Where the file of this blob lives, whether or not it exists.
    pub fn path_of(&self, hash: &H::Output, format: F) -> PathBuf {
        self.shared
            .root
            .join(rel_path::<N>(hash, format.extension()))
    }

    /// Start streaming a new file into `tmp/`. Safe inside a
    /// [`BlobStore::write`] closure.
    pub fn begin(&self) -> Result<Ingest<F, H>, Error> {
        let temp = Temp::create(Arc::clone(&self.shared), &self.shared.root.join(TMP_DIR))?;
        Ok(Ingest::new(temp))
    }

    /// Stream `bytes` into `tmp/` in one call.
    pub fn save(&self, bytes: &[u8]) -> Result<Staged<F, H>, Error> {
        let mut ingest = self.begin()?;
        ingest.write(bytes)?;
        ingest.finish()
    }

    /// Rename a staged file into its place in the tree, and fsync it there.
    ///
    /// Fails with [`Error::UnknownFormat`] when sniffing found no format; the
    /// temp file is deleted then. Do not call this inside a
    /// [`BlobStore::write`] closure: it takes the file lock, and the
    /// catalog is held there.
    pub fn land(&self, staged: Staged<F, H>) -> Result<Landed<F, H>, Error> {
        if !Arc::ptr_eq(&staged.temp.shared, &self.shared) {
            return Err(Error::WrongStore);
        }
        let format = staged.format().ok_or(Error::UnknownFormat)?;
        let hash = staged.hash();
        let path = self.path_of(&hash, format);
        let dir = path.parent().expect("a blob path has a parent");

        let _files = self.shared.lock_files();
        // From here on, a failure drops `landed`, which clears the mark.
        let landed = Landed::new(
            Arc::clone(&self.shared),
            hash,
            format,
            staged.size_bytes(),
            path.clone(),
        );
        if !self.shared.is_durable_dir(dir) {
            create_shard(&self.shared.root, dir)?;
            self.shared.mark_durable_dir(dir.to_owned());
        }
        // Replacing an existing file is safe: it holds these same bytes,
        // because the name is the hash of the content.
        staged.temp.persist(&path)?;
        sync_dir(dir)?;
        Ok(landed)
    }

    /// Run `f` in one catalog transaction, and commit when it returns `Ok`.
    ///
    /// After the commit, delete the files of the records that `f` removed.
    /// A file that fails to go is listed in [`Written::failed_removals`],
    /// and stays until the next sweep.
    ///
    /// The catalog is held while `f` runs. So inside `f`, do not call these
    /// methods of the same store, or of the same catalog: they can block
    /// forever.
    ///
    /// - [`BlobStore::write`], [`BlobStore::get`], [`BlobStore::list`], and
    ///   [`BlobStore::missing`] take the catalog again. A catalog behind a
    ///   plain mutex, like [`crate::sqlite::Sqlite`], waits for itself. Read
    ///   inside the transaction with [`BlobTx::get`] instead.
    /// - [`BlobStore::land`] and [`BlobStore::sweep`] take the file lock, out
    ///   of order, and can block against a sweep that holds it and waits for
    ///   the catalog.
    ///
    /// [`BlobStore::begin`], [`BlobStore::save`], and dropping an
    /// [`Ingest`], a [`Staged`], or a [`Landed`] are safe inside `f`.
    pub fn write<T, E: From<Error>>(
        &self,
        f: impl FnOnce(&mut BlobTx<'_, '_, C, F, H>) -> Result<T, E>,
    ) -> Result<Written<T, H::Output>, E> {
        let result = self.catalog.write(|tx| {
            let mut blob_tx = BlobTx::new(tx, &self.shared);
            let value = f(&mut blob_tx).map_err(Failure::App)?;
            Ok::<_, Failure<E, C::Error>>((value, blob_tx.into_parts()))
        });
        let (value, (landed, removed)) = match result {
            Ok(done) => done,
            Err(Failure::App(error)) => return Err(error),
            Err(Failure::Catalog(error)) => return Err(Error::catalog(error).into()),
        };
        // Committed: the records now protect their files.
        drop(landed);
        let failed_removals = removed
            .into_iter()
            .filter_map(|(hash, format)| self.remove_file(hash, format).err())
            .collect();
        Ok(Written {
            value,
            failed_removals,
        })
    }

    /// Delete the file of a record that a committed transaction removed,
    /// unless the blob came back in the meantime.
    fn remove_file(&self, hash: H::Output, format: F) -> Result<(), FailedRemoval<H::Output>> {
        let path = self.path_of(&hash, format);
        let _files = self.shared.lock_files();
        if self.shared.is_in_flight(&hash) {
            return Ok(());
        }
        let fail = |error| FailedRemoval {
            hash,
            path: path.clone(),
            error,
        };
        match self.catalog.get(&hash) {
            Ok(Some(found)) if found == format => return Ok(()),
            Ok(_) => {},
            Err(error) => return Err(fail(Error::catalog(error))),
        }
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(fail(error.into())),
        }
    }

    /// The format of a blob's record, as last committed.
    pub fn get(&self, hash: &H::Output) -> Result<Option<F>, Error> {
        self.catalog.get(hash).map_err(Error::catalog)
    }

    /// Every record, as last committed, in hash order.
    pub fn list(&self) -> Result<Vec<(H::Output, F)>, Error> {
        self.catalog.list().map_err(Error::catalog)
    }

    /// Records whose file is absent: damage from outside the store.
    pub fn missing(&self) -> Result<Vec<(H::Output, F)>, Error> {
        let mut missing = Vec::new();
        for (hash, format) in self.list()? {
            if !self.path_of(&hash, format).try_exists()? {
                missing.push((hash, format));
            }
        }
        Ok(missing)
    }

    pub(crate) fn shared(&self) -> &Shared<H::Output> {
        &self.shared
    }
}

enum Failure<E, CE> {
    App(E),
    Catalog(CE),
}

impl<E, CE> From<CE> for Failure<E, CE> {
    fn from(error: CE) -> Self {
        Self::Catalog(error)
    }
}

/// The path of a blob under the root: `N` directory levels named by the
/// leading bytes of the hash, then `{hash}.{extension}`.
pub(crate) fn rel_path<const N: usize>(hash: &impl HashOutput, extension: &str) -> PathBuf {
    let hex = hash.to_string();
    let mut path: PathBuf = (0..N).map(|level| &hex[2 * level..2 * level + 2]).collect();
    path.push(format!("{hex}.{extension}"));
    path
}

/// Write the layout file on first use, and check it after that.
fn check_layout(root: &Path, expected: &str) -> Result<(), Error> {
    let path = root.join(LAYOUT_FILE);
    match fs::read_to_string(&path) {
        Ok(found) if found == expected => Ok(()),
        Ok(found) => Err(Error::Layout {
            expected: expected.to_owned(),
            found,
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let temp = root.join(TMP_DIR).join(LAYOUT_FILE);
            fs::write(&temp, expected)?;
            File::open(&temp)?.sync_all()?;
            fs::rename(&temp, &path)?;
            sync_dir(root)?;
            Ok(())
        },
        Err(error) => Err(error.into()),
    }
}

/// Create the root, its `tmp/`, and any missing ancestors, and make them
/// durable: a record must never commit for a file whose directory a crash
/// could lose.
fn create_root(root: &Path) -> io::Result<()> {
    let mut missing = Vec::new();
    let mut current = root;
    while !current.try_exists()? {
        missing.push(current);
        match parent_of(current) {
            Some(parent) => current = parent,
            None => break,
        }
    }
    fs::create_dir_all(root.join(TMP_DIR))?;
    for created in missing.iter().rev() {
        if let Some(parent) = parent_of(created) {
            sync_dir(parent)?;
        }
    }
    // Always, not only when something was created: an earlier open may have
    // created the root, then failed to sync it.
    if let Some(parent) = parent_of(root) {
        sync_dir(parent)?;
    }
    sync_dir(root)
}

/// Create a shard directory, and sync every directory from its parent up to
/// the root, so its entries survive a crash.
fn create_shard(root: &Path, dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let mut current = dir;
    while current != root {
        current = current.parent().expect("a shard lies under the root");
        sync_dir(current)?;
    }
    Ok(())
}

/// The parent directory, with `.` for a bare relative name. `None` at the
/// top of the file system.
fn parent_of(path: &Path) -> Option<&Path> {
    match path.parent()? {
        parent if parent.as_os_str().is_empty() => Some(Path::new(".")),
        parent => Some(parent),
    }
}

/// Make a directory's entries durable.
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(test)]
    sync_hook::observe(dir)?;
    File::open(dir)?.sync_all()
}

/// Lets a test watch directory syncs, and make some of them fail.
#[cfg(test)]
pub(crate) mod sync_hook {
    use std::{
        cell::{Cell, RefCell},
        io,
        path::{Path, PathBuf},
    };

    thread_local! {
        static SYNCED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
        static FAILURES: Cell<usize> = const { Cell::new(0) };
    }

    pub fn observe(dir: &Path) -> io::Result<()> {
        if FAILURES.get() > 0 {
            FAILURES.set(FAILURES.get() - 1);
            return Err(io::Error::other("injected sync failure"));
        }
        SYNCED.with_borrow_mut(|synced| synced.push(dir.to_owned()));
        Ok(())
    }

    /// Make the next `count` syncs on this thread fail.
    // Only the store tests use it, and they need some features.
    #[allow(dead_code)]
    pub fn fail_next(count: usize) {
        FAILURES.set(count);
    }

    /// The directories synced on this thread since the last call.
    // Only the store tests use it, and they need some features.
    #[allow(dead_code)]
    pub fn take() -> Vec<PathBuf> {
        SYNCED.with_borrow_mut(std::mem::take)
    }
}

#[cfg(all(test, feature = "format-presets", feature = "hasher-blake3"))]
mod tests {
    use {
        super::*,
        crate::Blake3,
        crate::{
            Digest, Error, Inserted,
            testing::{Mem, TestFormat, at, pdf, png},
        },
        std::{sync::Arc, thread},
        tempfile::TempDir,
    };

    type Hash = Digest<32>;
    type Store = BlobStore<Mem<Hash>, TestFormat>;

    fn open() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("blobs"), Mem::default()).unwrap();
        (dir, store)
    }

    /// Save, land, and record `bytes` in one go.
    fn add(store: &Store, bytes: &[u8]) -> Hash {
        let landed = store.land(store.save(bytes).unwrap()).unwrap();
        let hash = landed.hash();
        store
            .write(|tx| tx.insert(landed, at()))
            .unwrap()
            .into_value();
        hash
    }

    fn tmp_entries(store: &Store) -> usize {
        fs::read_dir(store.root().join(TMP_DIR)).unwrap().count()
    }

    fn record(store: &Store, hash: &Hash) -> Option<TestFormat> {
        store.get(hash).unwrap()
    }

    /// P1, checked over every record: each one has its file.
    fn assert_records_have_files(store: &Store) {
        assert_eq!(store.missing().unwrap(), []);
    }

    // ------------------------------------------------------------------ layout

    #[test]
    fn paths_take_one_level_per_leading_byte() {
        let hash = Hash::from_bytes(std::array::from_fn(|i| i as u8 + 1));
        let hex = hash.to_string();
        assert_eq!(
            rel_path::<0>(&hash, "png"),
            PathBuf::from(format!("{hex}.png"))
        );
        assert_eq!(
            rel_path::<2>(&hash, "png"),
            PathBuf::from(format!("01/02/{hex}.png"))
        );
        assert_eq!(
            rel_path::<3>(&hash, "pdf"),
            PathBuf::from(format!("01/02/03/{hex}.pdf"))
        );
    }

    #[test]
    fn open_creates_the_root_and_its_layout() {
        let (_dir, store) = open();
        assert!(store.root().join(TMP_DIR).is_dir());
        assert!(store.root().join(LOCK_FILE).is_file());
        assert_eq!(
            fs::read_to_string(store.root().join(LAYOUT_FILE)).unwrap(),
            "hasher = \"blake3\"\ndepth  = 2\n"
        );
    }

    #[test]
    fn open_makes_a_new_root_and_its_new_ancestors_durable() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("a").join("b").join("blobs");
        sync_hook::take();
        let _store = Store::open(&root, Mem::default()).unwrap();
        let synced = sync_hook::take();
        for expected in [
            dir.path().to_owned(),
            dir.path().join("a"),
            dir.path().join("a").join("b"),
            root.clone(),
        ] {
            assert!(synced.contains(&expected), "{expected:?} not in {synced:?}");
        }
        assert!(root.join(TMP_DIR).is_dir());
    }

    #[test]
    fn open_syncs_the_root_again_after_a_failed_open() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("blobs");
        sync_hook::fail_next(1);
        assert!(matches!(
            Store::open(&root, Mem::default()),
            Err(Error::Io(_))
        ));
        assert!(root.is_dir(), "the root was created before the sync failed");

        sync_hook::take();
        let _store = Store::open(&root, Mem::default()).unwrap();
        let synced = sync_hook::take();
        assert!(synced.contains(&dir.path().to_owned()), "{synced:?}");
        assert!(synced.contains(&root), "{synced:?}");
    }

    #[test]
    fn a_new_shard_is_synced_up_to_the_root() {
        let (_dir, store) = open();
        sync_hook::take();
        let landed = store.land(store.save(&png(19)).unwrap()).unwrap();
        let shard = landed.path().parent().unwrap().to_owned();
        let synced = sync_hook::take();
        assert_eq!(
            synced,
            [
                shard.parent().unwrap().to_owned(),
                store.root().to_owned(),
                shard
            ]
        );
    }

    #[test]
    fn a_failed_shard_sync_is_retried_by_the_next_land() {
        let (_dir, store) = open();
        let shard = store.path_of(&Blake3::digest(&png(20)), TestFormat::Png);
        let shard = shard.parent().unwrap().to_owned();

        // The shard is created, but syncing its parent fails.
        sync_hook::fail_next(1);
        assert!(matches!(
            store.land(store.save(&png(20)).unwrap()),
            Err(Error::Io(_))
        ));
        assert!(shard.is_dir());

        // The shard exists now, but the next land syncs its parents again.
        sync_hook::take();
        let landed = store.land(store.save(&png(20)).unwrap()).unwrap();
        let synced = sync_hook::take();
        assert_eq!(
            synced,
            [
                shard.parent().unwrap().to_owned(),
                store.root().to_owned(),
                shard.clone()
            ]
        );

        // Once that succeeded, a land into the shard syncs only the shard.
        drop(landed);
        store.land(store.save(&png(20)).unwrap()).unwrap();
        assert_eq!(sync_hook::take(), [shard]);
    }

    #[test]
    fn a_store_reopens_with_the_same_settings() {
        let (dir, store) = open();
        let hash = add(&store, &png(1));
        let path = store.path_of(&hash, TestFormat::Png);
        drop(store);

        let store = Store::open(dir.path().join("blobs"), Mem::default()).unwrap();
        assert_eq!(store.path_of(&hash, TestFormat::Png), path);
        assert!(path.is_file());
    }

    #[test]
    fn a_second_handle_is_refused_while_the_first_is_open() {
        let (dir, store) = open();
        let root = dir.path().join("blobs");
        assert!(matches!(
            Store::open(&root, Mem::default()),
            Err(Error::Locked(path)) if path == root
        ));
        drop(store);
        Store::open(&root, Mem::default()).unwrap();
    }

    #[test]
    fn another_depth_is_refused() {
        let (dir, store) = open();
        drop(store);
        let result = BlobStore::<Mem<Hash>, TestFormat, Blake3, 3>::open_with(
            dir.path().join("blobs"),
            Mem::default(),
        );
        assert!(matches!(result, Err(Error::Layout { .. })));
    }

    #[cfg(feature = "hasher-sha256")]
    #[test]
    fn another_hasher_is_refused() {
        let (dir, store) = open();
        drop(store);
        let result = BlobStore::<Mem<Hash>, TestFormat, crate::Sha256>::open_with(
            dir.path().join("blobs"),
            Mem::default(),
        );
        assert!(matches!(result, Err(Error::Layout { .. })));
    }

    #[cfg(feature = "hasher-sha256")]
    #[test]
    fn other_hashers_and_depths_work_end_to_end() {
        let dir = TempDir::new().unwrap();
        let store = BlobStore::<Mem<Hash>, TestFormat, crate::Sha256, 3>::open_with(
            dir.path(),
            Mem::default(),
        )
        .unwrap();
        let landed = store.land(store.save(&pdf(1)).unwrap()).unwrap();
        let hash = landed.hash();
        store.write(|tx| tx.insert(landed, at())).unwrap();

        assert_eq!(hash, crate::Sha256::digest(&pdf(1)));
        let hex = hash.to_string();
        let expected = dir
            .path()
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(&hex[4..6])
            .join(format!("{hex}.pdf"));
        assert_eq!(fs::read(expected).unwrap(), pdf(1));
    }

    // -------------------------------------------------------------------- save

    #[test]
    fn a_saved_blob_lands_in_its_place_and_is_recorded() {
        let (_dir, store) = open();
        let staged = store.save(&png(1)).unwrap();
        assert_eq!(staged.hash(), Blake3::digest(&png(1)));
        assert_eq!(staged.size_bytes(), png(1).len() as u64);
        assert_eq!(staged.format(), Some(TestFormat::Png));

        let landed = store.land(staged).unwrap();
        let hash = landed.hash();
        let path = landed.path().to_owned();
        assert_eq!(path, store.path_of(&hash, TestFormat::Png));
        assert_eq!(fs::read(&path).unwrap(), png(1));

        let written = store.write(|tx| tx.insert(landed, at())).unwrap();
        assert_eq!(written.value, Inserted::New);
        assert!(written.failed_removals.is_empty());
        assert_eq!(record(&store, &hash), Some(TestFormat::Png));
        assert_eq!(tmp_entries(&store), 0);
        assert_records_have_files(&store);
    }

    #[test]
    fn chunking_does_not_change_the_hash_or_the_format() {
        let (_dir, store) = open();
        let payload = pdf(2);
        let mut ingest = store.begin().unwrap();
        for chunk in payload.chunks(3) {
            ingest.write(chunk).unwrap();
        }
        let staged = ingest.finish().unwrap();
        assert_eq!(staged.hash(), Blake3::digest(&payload));
        assert_eq!(staged.format(), Some(TestFormat::Pdf));
        assert_eq!(staged.size_bytes(), payload.len() as u64);
    }

    #[test]
    fn sniffing_sees_only_the_head() {
        let (_dir, store) = open();
        // A format marker past the sniffed head does not count.
        let mut payload = vec![0; crate::SNIFF_LEN];
        payload.extend(png(3));
        assert_eq!(store.save(&payload).unwrap().format(), None);
    }

    #[test]
    fn an_unknown_format_cannot_land() {
        let (_dir, store) = open();
        for payload in [&b"just some text"[..], &[]] {
            let staged = store.save(payload).unwrap();
            assert_eq!(staged.format(), None);
            assert!(matches!(store.land(staged), Err(Error::UnknownFormat)));
        }
        assert_eq!(tmp_entries(&store), 0);
        let entries: Vec<_> = fs::read_dir(store.root()).unwrap().collect();
        assert_eq!(entries.len(), 3, "only tmp/, the layout, and the lock");
    }

    #[test]
    fn dropping_an_ingest_or_a_staged_file_deletes_the_temp_file() {
        let (_dir, store) = open();
        let mut ingest = store.begin().unwrap();
        ingest.write(b"partial").unwrap();
        assert_eq!(tmp_entries(&store), 1);
        drop(ingest);
        assert_eq!(tmp_entries(&store), 0);

        let staged = store.save(&png(4)).unwrap();
        assert_eq!(tmp_entries(&store), 1);
        drop(staged);
        assert_eq!(tmp_entries(&store), 0);
    }

    #[test]
    fn a_failed_write_fails_the_ingest_for_good() {
        let (_dir, store) = open();
        let hash = add(&store, &png(18));
        let path = store.path_of(&hash, TestFormat::Png);

        // The same bytes again, but the first write puts down only part of
        // its chunk before it fails.
        let payload = png(18);
        let mut ingest = store.begin().unwrap();
        ingest.fail_after = Some(5);
        assert!(matches!(ingest.write(&payload), Err(Error::Io(_))));

        // A retry would leave bytes in the file that the hash never saw.
        assert!(matches!(ingest.write(&payload), Err(Error::IngestFailed)));
        assert!(matches!(ingest.finish(), Err(Error::IngestFailed)));

        // The recorded file is untouched, and the temp file is gone.
        assert_eq!(fs::read(&path).unwrap(), png(18));
        assert_eq!(tmp_entries(&store), 0);
        assert_records_have_files(&store);
    }

    #[test]
    fn saving_the_same_bytes_twice_keeps_one_file_and_one_record() {
        let (_dir, store) = open();
        let hash = add(&store, &png(5));
        let landed = store.land(store.save(&png(5)).unwrap()).unwrap();
        let written = store.write(|tx| tx.insert(landed, at())).unwrap();
        assert_eq!(written.value, Inserted::AlreadyPresent);
        assert_eq!(store.list().unwrap(), [(hash, TestFormat::Png)]);
        assert_eq!(
            fs::read(store.path_of(&hash, TestFormat::Png)).unwrap(),
            png(5)
        );
        assert_eq!(tmp_entries(&store), 0);
    }

    #[test]
    fn a_failed_closure_records_nothing_and_leaves_an_orphan() {
        let (_dir, store) = open();
        let landed = store.land(store.save(&png(6)).unwrap()).unwrap();
        let hash = landed.hash();
        let result: Result<Written<(), Hash>, Error> = store.write(|tx| {
            tx.insert(landed, at())?;
            Err(Error::UnknownFormat)
        });
        assert!(result.is_err());
        assert_eq!(record(&store, &hash), None);

        let report = store.sweep().unwrap();
        assert_eq!(
            report.removed_orphans,
            [store.path_of(&hash, TestFormat::Png)]
        );
    }

    #[test]
    fn a_failed_commit_records_nothing_and_leaves_an_orphan() {
        let (_dir, store) = open();
        let landed = store.land(store.save(&png(7)).unwrap()).unwrap();
        let hash = landed.hash();
        store.catalog().fail_commits(true);
        assert!(matches!(
            store.write(|tx| tx.insert(landed, at())),
            Err(Error::Catalog(_))
        ));
        store.catalog().fail_commits(false);
        assert_eq!(record(&store, &hash), None);
        assert_eq!(store.sweep().unwrap().removed_orphans.len(), 1);
    }

    #[test]
    fn a_dropped_landed_file_is_an_orphan() {
        let (_dir, store) = open();
        let landed = store.land(store.save(&png(8)).unwrap()).unwrap();
        let path = landed.path().to_owned();
        drop(landed);
        assert_eq!(store.sweep().unwrap().removed_orphans, [path]);
    }

    #[test]
    fn a_landed_file_survives_a_sweep_until_it_is_recorded() {
        let (_dir, store) = open();
        let landed = store.land(store.save(&png(9)).unwrap()).unwrap();
        let path = landed.path().to_owned();
        let report = store.sweep().unwrap();
        assert!(report.removed_orphans.is_empty());
        assert!(path.is_file());

        store.write(|tx| tx.insert(landed, at())).unwrap();
        assert!(store.sweep().unwrap().removed_orphans.is_empty());
        assert!(path.is_file());
    }

    #[test]
    fn dropping_a_landed_file_inside_a_write_does_not_deadlock() {
        let (_dir, store) = open();
        let landed = store.land(store.save(&png(10)).unwrap()).unwrap();
        store
            .write(|_| {
                drop(landed);
                Ok::<_, Error>(())
            })
            .unwrap();
        assert_eq!(store.sweep().unwrap().removed_orphans.len(), 1);
    }

    #[test]
    fn a_blob_from_another_store_is_refused() {
        let (_dir_a, a) = open();
        let (_dir_b, b) = open();
        assert!(matches!(
            b.land(a.save(&png(11)).unwrap()),
            Err(Error::WrongStore)
        ));
        let landed = a.land(a.save(&png(11)).unwrap()).unwrap();
        assert!(matches!(
            b.write(|tx| tx.insert(landed, at())),
            Err(Error::WrongStore)
        ));
        assert!(b.list().unwrap().is_empty());
    }

    // ------------------------------------------------------------------ remove

    #[test]
    fn remove_deletes_the_record_and_then_the_file() {
        let (_dir, store) = open();
        let hash = add(&store, &png(12));
        let path = store.path_of(&hash, TestFormat::Png);

        let written = store.write(|tx| tx.remove(&hash)).unwrap();
        assert_eq!(written.value, Some(TestFormat::Png));
        assert!(written.failed_removals.is_empty());
        assert_eq!(record(&store, &hash), None);
        assert!(!path.exists());
    }

    #[test]
    fn removing_an_unknown_hash_changes_nothing() {
        let (_dir, store) = open();
        let kept = add(&store, &png(13));
        let unknown = Blake3::digest(b"never saved");
        let written = store.write(|tx| tx.remove(&unknown)).unwrap();
        assert_eq!(written.value, None);
        assert_eq!(record(&store, &kept), Some(TestFormat::Png));
        assert_records_have_files(&store);
    }

    #[test]
    fn a_rolled_back_remove_keeps_the_record_and_the_file() {
        let (_dir, store) = open();
        let hash = add(&store, &png(14));
        let result: Result<Written<(), Hash>, Error> = store.write(|tx| {
            tx.remove(&hash)?;
            Err(Error::UnknownFormat)
        });
        assert!(result.is_err());
        assert_eq!(record(&store, &hash), Some(TestFormat::Png));
        assert_records_have_files(&store);
    }

    #[test]
    fn remove_keeps_a_file_that_is_landed_again() {
        let (_dir, store) = open();
        let hash = add(&store, &png(15));
        // A second save of the same bytes lands, but is not recorded yet.
        let again = store.land(store.save(&png(15)).unwrap()).unwrap();

        store.write(|tx| tx.remove(&hash)).unwrap();
        assert!(store.path_of(&hash, TestFormat::Png).is_file());

        store.write(|tx| tx.insert(again, at())).unwrap();
        assert_eq!(record(&store, &hash), Some(TestFormat::Png));
        assert_records_have_files(&store);
    }

    #[test]
    fn remove_keeps_a_file_whose_record_comes_back_in_the_same_transaction() {
        let (_dir, store) = open();
        let hash = add(&store, &png(16));
        let again = store.land(store.save(&png(16)).unwrap()).unwrap();
        store
            .write(|tx| {
                tx.remove(&hash)?;
                assert_eq!(tx.get(&hash)?, None);
                tx.insert(again, at())
            })
            .unwrap();
        assert_eq!(record(&store, &hash), Some(TestFormat::Png));
        assert_records_have_files(&store);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_file_deletion_is_reported_and_left_for_the_sweep() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, store) = open();
        let hash = add(&store, &png(17));
        let path = store.path_of(&hash, TestFormat::Png);
        let shard = path.parent().unwrap().to_owned();

        fs::set_permissions(&shard, fs::Permissions::from_mode(0o555)).unwrap();
        let written = store.write(|tx| tx.remove(&hash)).unwrap();
        fs::set_permissions(&shard, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(written.failed_removals.len(), 1);
        assert_eq!(written.failed_removals[0].hash, hash);
        assert_eq!(written.failed_removals[0].path, path);
        assert_eq!(record(&store, &hash), None);
        assert!(path.is_file());

        assert_eq!(store.sweep().unwrap().removed_orphans, [path]);
    }

    // ------------------------------------------------------------------- sweep

    #[test]
    fn sweep_deletes_what_is_provably_ours_and_lists_the_rest() {
        let (_dir, store) = open();
        let root = store.root().to_owned();

        // A recorded file: kept.
        let known = add(&store, &png(20));
        let known_path = store.path_of(&known, TestFormat::Png);

        // A file with no record: deleted.
        let orphan = store.land(store.save(&png(21)).unwrap()).unwrap();
        let orphan_path = orphan.path().to_owned();
        drop(orphan);

        // A file whose record names another format: deleted.
        let other = store.land(store.save(&pdf(22)).unwrap()).unwrap();
        let other_path = other.path().to_owned();
        store.catalog().insert_raw(other.hash(), TestFormat::Png);
        drop(other);

        // A temp file nobody owns: deleted. One that an ingest owns: kept.
        fs::write(root.join(TMP_DIR).join("leftover"), b"x").unwrap();
        let live = store.begin().unwrap();

        // Things the store never writes: kept and listed.
        let stray = root.join("read_me.txt");
        fs::write(&stray, b"not ours").unwrap();
        let upper_shard = root.join("AB");
        fs::create_dir(&upper_shard).unwrap();
        let bad_stem = root.join("aa").join("bb").join("zz.png");
        fs::create_dir_all(bad_stem.parent().unwrap()).unwrap();
        fs::write(&bad_stem, b"x").unwrap();
        let unknown_ext = root.join("aa").join("bb").join(format!("{known}.gif"));
        fs::write(&unknown_ext, b"x").unwrap();
        let misplaced = root.join("00").join("00").join(format!("{known}.png"));
        fs::create_dir_all(misplaced.parent().unwrap()).unwrap();
        fs::write(&misplaced, b"x").unwrap();
        let leaf_dir = known_path.parent().unwrap().join("extra");
        fs::create_dir(&leaf_dir).unwrap();

        let report = store.sweep().unwrap();

        assert_eq!(report.removed_temp, 1);
        let mut orphans = vec![orphan_path.clone(), other_path.clone()];
        orphans.sort();
        assert_eq!(report.removed_orphans, orphans);
        let mut unrecognized = vec![
            stray,
            upper_shard,
            bad_stem,
            unknown_ext,
            misplaced,
            leaf_dir,
        ];
        unrecognized.sort();
        assert_eq!(report.unrecognized, unrecognized);
        assert!(report.failed.is_empty());

        assert!(known_path.is_file());
        assert!(!orphan_path.exists());
        assert!(!other_path.exists());
        assert_eq!(tmp_entries(&store), 1, "the live ingest keeps its file");
        drop(live);
        assert_eq!(tmp_entries(&store), 0);
    }

    #[test]
    fn sweep_works_at_every_depth() {
        fn check<const N: usize>() {
            let dir = TempDir::new().unwrap();
            let store = BlobStore::<Mem<Hash>, TestFormat, Blake3, N>::open_with(
                dir.path(),
                Mem::default(),
            )
            .unwrap();
            let kept = store.land(store.save(&png(1)).unwrap()).unwrap();
            let kept_path = kept.path().to_owned();
            store.write(|tx| tx.insert(kept, at())).unwrap();
            let orphan = store.land(store.save(&png(2)).unwrap()).unwrap();
            let orphan_path = orphan.path().to_owned();
            drop(orphan);

            let report = store.sweep().unwrap();
            assert_eq!(report.removed_orphans, [orphan_path], "depth {N}");
            assert!(report.unrecognized.is_empty(), "depth {N}");
            assert!(kept_path.is_file(), "depth {N}");
        }
        check::<0>();
        check::<1>();
        check::<2>();
        check::<4>();
    }

    #[test]
    fn missing_lists_records_whose_file_is_gone() {
        let (_dir, store) = open();
        let present = add(&store, &png(30));
        let gone = add(&store, &pdf(31));
        fs::remove_file(store.path_of(&gone, TestFormat::Pdf)).unwrap();
        assert_eq!(store.missing().unwrap(), [(gone, TestFormat::Pdf)]);
        assert_eq!(record(&store, &present), Some(TestFormat::Png));
    }

    // ----------------------------------------------------------------- sharing

    #[test]
    fn an_arc_catalog_is_shared_with_the_app() {
        let dir = TempDir::new().unwrap();
        let catalog = Arc::new(Mem::<Hash>::default());
        let store = BlobStore::<_, TestFormat>::open(dir.path(), Arc::clone(&catalog)).unwrap();
        let landed = store.land(store.save(&png(40)).unwrap()).unwrap();
        let hash = landed.hash();
        store.write(|tx| tx.insert(landed, at())).unwrap();
        assert_eq!(
            Catalog::<TestFormat, Blake3>::get(&*catalog, &hash).unwrap(),
            Some(TestFormat::Png)
        );
    }

    /// `begin` and `save` are safe inside a write closure, even while a sweep
    /// holds the file lock and waits for the catalog.
    #[test]
    fn saving_inside_a_write_does_not_block_against_a_sweep() {
        let (_dir, store) = open();
        // Files for the sweep to walk: each one takes the file lock, then
        // reads the catalog.
        for n in 0..40 {
            add(&store, &pdf(n));
        }
        let (done, finished) = std::sync::mpsc::channel();
        let stop = std::sync::atomic::AtomicBool::new(false);
        thread::scope(|scope| {
            let store = &store;
            let stop = &stop;
            scope.spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    store.sweep().unwrap();
                }
            });
            scope.spawn(move || {
                for n in 0..200 {
                    store
                        .write(|_| {
                            let staged = store.save(&png(n as u8))?;
                            drop(staged);
                            Ok::<_, Error>(())
                        })
                        .unwrap();
                }
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                done.send(()).unwrap();
            });
            if finished
                .recv_timeout(std::time::Duration::from_secs(30))
                .is_err()
            {
                // The threads are stuck; a scope would wait for them forever.
                eprintln!("deadlock: a save inside a write blocked against a sweep");
                std::process::abort();
            }
        });
    }

    /// Saves, deletes, and sweeps of the same bytes race on several threads.
    /// Whenever they all stop, every record must have its file. Chance rarely
    /// hits the narrow windows; the tests above pin those down one by one.
    #[test]
    fn racing_saves_deletes_and_sweeps_keep_records_and_files_in_sync() {
        let (_dir, store) = open();
        let payloads: Vec<Vec<u8>> = (0..3).map(png).collect();
        let hashes: Vec<Hash> = payloads.iter().map(|p| Blake3::digest(p)).collect();

        for _ in 0..10 {
            thread::scope(|scope| {
                for worker in 0..2 {
                    let store = &store;
                    let payloads = &payloads;
                    scope.spawn(move || {
                        for round in 0..20 {
                            let payload = &payloads[(worker + round) % payloads.len()];
                            let landed = store.land(store.save(payload).unwrap()).unwrap();
                            store.write(|tx| tx.insert(landed, at())).unwrap();
                        }
                    });
                }
                let store = &store;
                let hashes = &hashes;
                scope.spawn(move || {
                    for round in 0..20 {
                        let hash = &hashes[round % hashes.len()];
                        let written = store.write(|tx| tx.remove(hash)).unwrap();
                        assert!(written.failed_removals.is_empty());
                    }
                });
                scope.spawn(move || {
                    for _ in 0..5 {
                        let report = store.sweep().unwrap();
                        assert!(report.unrecognized.is_empty());
                        assert!(report.failed.is_empty());
                    }
                });
            });
            assert_records_have_files(&store);
        }
    }
}
