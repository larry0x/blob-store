use {
    crate::{
        BlobStore, Catalog, Error, Format, Hasher,
        store::{LAYOUT_FILE, LOCK_FILE, TMP_DIR},
    },
    std::{ffi::OsStr, fs, io, path::PathBuf},
};

/// What a sweep did.
#[derive(Debug, Default)]
pub struct SweepReport {
    /// Temp files left behind by a crash or a lost process.
    pub removed_temp: usize,
    /// Files with a well-formed name in the right place, but no record.
    pub removed_orphans: Vec<PathBuf>,
    /// Files the store can't prove to be its own. They are kept.
    pub unrecognized: Vec<PathBuf>,
    /// Deletions that failed. Each file stays until the next sweep.
    pub failed: Vec<(PathBuf, io::Error)>,
}

impl<C: Catalog<F, H>, F: Format, H: Hasher, const N: usize> BlobStore<C, F, H, N> {
    /// Delete the temp files nobody owns, and the files that have no record.
    /// Keep and list everything else.
    ///
    /// Safe to run at any time: saves and deletes in progress are left
    /// alone. A failed deletion is listed, and the sweep goes on. A failed
    /// catalog read stops the sweep, since nothing can then be proven an
    /// orphan.
    pub fn sweep(&self) -> Result<SweepReport, Error> {
        let mut report = SweepReport::default();

        for entry in fs::read_dir(self.root().join(TMP_DIR))? {
            let entry = entry?;
            match self
                .shared()
                .remove_temp_unless_live(&entry.path(), &entry.file_name())
            {
                Ok(true) => report.removed_temp += 1,
                Ok(false) => {},
                Err(error) => report.failed.push((entry.path(), error)),
            }
        }

        for entry in fs::read_dir(self.root())? {
            let entry = entry?;
            let name = entry.file_name();
            if [TMP_DIR, LAYOUT_FILE, LOCK_FILE]
                .map(OsStr::new)
                .contains(&name.as_os_str())
            {
                continue;
            }
            self.sweep_entry(entry.path(), entry.file_type()?.is_dir(), 0, &mut report)?;
        }

        // read_dir order depends on the file system; sort for determinism.
        report.removed_orphans.sort();
        report.unrecognized.sort();
        report.failed.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(report)
    }

    /// `level` counts the directories between the root and `path`.
    fn sweep_entry(
        &self,
        path: PathBuf,
        is_dir: bool,
        level: usize,
        report: &mut SweepReport,
    ) -> Result<(), Error> {
        if level == N {
            return if is_dir {
                report.unrecognized.push(path);
                Ok(())
            } else {
                self.sweep_file(path, report)
            };
        }
        let is_shard = path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(is_shard_name);
        if !is_dir || !is_shard {
            report.unrecognized.push(path);
            return Ok(());
        }
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            self.sweep_entry(entry.path(), entry.file_type()?.is_dir(), level + 1, report)?;
        }
        Ok(())
    }

    fn sweep_file(&self, path: PathBuf, report: &mut SweepReport) -> Result<(), Error> {
        let parsed = path
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(parse_file_name::<F, H>);
        let Some((hash, format)) = parsed else {
            report.unrecognized.push(path);
            return Ok(());
        };
        // A well-formed name in the wrong place is not a file the store
        // ever writes.
        if path != self.path_of(&hash, format) {
            report.unrecognized.push(path);
            return Ok(());
        }

        let _files = self.shared().lock_files();
        if self.shared().is_in_flight(&hash) {
            return Ok(());
        }
        if self.catalog().get(&hash).map_err(Error::catalog)? == Some(format) {
            return Ok(());
        }
        match fs::remove_file(&path) {
            Ok(()) => report.removed_orphans.push(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => report.failed.push((path, error)),
        }
        Ok(())
    }
}

/// Two lowercase hex characters: the only directory names the store makes.
fn is_shard_name(name: &str) -> bool {
    name.len() == 2 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The inverse of the naming scheme: `{hash}.{extension}`.
fn parse_file_name<F: Format, H: Hasher>(name: &str) -> Option<(H::Output, F)> {
    let (stem, extension) = name.rsplit_once('.')?;
    Some((stem.parse().ok()?, F::from_extension(extension)?))
}
