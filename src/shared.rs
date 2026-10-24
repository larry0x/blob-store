use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs::File,
    hash::Hash,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, PoisonError},
};

/// The part of a store that its blobs in progress point back to.
///
/// Two locks guard it. The order is always: the file lock, then the catalog,
/// then the state lock. Nothing is taken while the state lock is held.
pub(crate) struct Shared<O> {
    pub root: PathBuf,
    /// Covers every step that adds a file to the tree or takes one away.
    files: Mutex<()>,
    state: Mutex<State<O>>,
    /// Holds the lock on the store's lock file while the store is open.
    _lock: File,
}

struct State<O> {
    /// Hashes landed but not yet committed or given up, with a count of
    /// each: two saves of the same bytes can be in flight at once.
    in_flight: HashMap<O, usize>,
    /// Names of the files in `tmp/` that an `Ingest` or a `Staged` owns.
    live_temps: HashSet<OsString>,
    /// Shard directories whose entries, up to the root, this process has
    /// synced. A directory that exists is not proof: an earlier sync of it
    /// may have failed.
    durable_dirs: HashSet<PathBuf>,
}

impl<O: Eq + Hash + Copy> Shared<O> {
    pub fn new(root: PathBuf, lock: File) -> Self {
        Self {
            root,
            files: Mutex::new(()),
            state: Mutex::new(State {
                in_flight: HashMap::new(),
                live_temps: HashSet::new(),
                durable_dirs: HashSet::new(),
            }),
            _lock: lock,
        }
    }

    // The data under both locks stays consistent at every step, so a panic
    // elsewhere while one was held leaves nothing to repair.

    pub fn lock_files(&self) -> MutexGuard<'_, ()> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn state(&self) -> MutexGuard<'_, State<O>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn mark(&self, hash: O) {
        *self.state().in_flight.entry(hash).or_insert(0) += 1;
    }

    pub fn unmark(&self, hash: &O) {
        let mut state = self.state();
        if let Some(count) = state.in_flight.get_mut(hash) {
            *count -= 1;
            if *count == 0 {
                state.in_flight.remove(hash);
            }
        }
    }

    pub fn is_in_flight(&self, hash: &O) -> bool {
        self.state().in_flight.contains_key(hash)
    }

    pub fn register_temp(&self, name: OsString) {
        self.state().live_temps.insert(name);
    }

    pub fn unregister_temp(&self, name: &OsStr) {
        self.state().live_temps.remove(name);
    }

    pub fn is_live_temp(&self, name: &OsStr) -> bool {
        self.state().live_temps.contains(name)
    }

    pub fn is_durable_dir(&self, dir: &Path) -> bool {
        self.state().durable_dirs.contains(dir)
    }

    pub fn mark_durable_dir(&self, dir: PathBuf) {
        self.state().durable_dirs.insert(dir);
    }
}
