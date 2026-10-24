use std::{error::Error as StdError, io, path::PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("file format not recognized")]
    UnknownFormat,
    #[error("the blob belongs to another store")]
    WrongStore,
    #[error("the store at {0} is open in another process")]
    Locked(PathBuf),
    #[error("the store was created as {found:?}, but opened as {expected:?}")]
    Layout { expected: String, found: String },
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Catalog(Box<dyn StdError + Send + Sync>),
}

impl Error {
    pub fn catalog(error: impl StdError + Send + Sync + 'static) -> Self {
        Self::Catalog(Box::new(error))
    }
}

impl From<tempfile::PersistError> for Error {
    fn from(error: tempfile::PersistError) -> Self {
        Self::Io(error.error)
    }
}
