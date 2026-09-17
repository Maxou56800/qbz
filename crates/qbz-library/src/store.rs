//! Profile-scoped access to a host's library database.
//!
//! The host selects the path once. This object never consults desktop globals,
//! last-user markers or environment defaults. Connections stay on the calling
//! blocking thread; reads of an absent library do not create a profile.

use std::path::{Path, PathBuf};

use crate::{LibraryDatabase, LibraryError};

#[derive(Debug, Clone)]
pub struct LibraryStore {
    database_path: PathBuf,
}

impl LibraryStore {
    pub fn new(database_path: PathBuf) -> Self {
        Self { database_path }
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Open an existing library, including its normal schema migrations.
    /// This is not a SQLite read-only connection: callers choose read operations.
    pub fn open_existing(&self) -> Result<Option<LibraryDatabase>, LibraryError> {
        if !self.database_path.try_exists()? {
            return Ok(None);
        }
        LibraryDatabase::open(&self.database_path).map(Some)
    }

    /// Open for an operation that may create this profile's library.
    pub fn open_or_create(&self) -> Result<LibraryDatabase, LibraryError> {
        if let Some(parent) = self
            .database_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        LibraryDatabase::open(&self.database_path)
    }

    pub fn read<T>(
        &self,
        operation: impl FnOnce(&LibraryDatabase) -> Result<T, LibraryError>,
    ) -> Result<Option<T>, LibraryError> {
        self.open_existing()?.as_ref().map(operation).transpose()
    }

    pub fn write<T>(
        &self,
        operation: impl FnOnce(&LibraryDatabase) -> Result<T, LibraryError>,
    ) -> Result<T, LibraryError> {
        operation(&self.open_or_create()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_read_does_not_create_profile_or_run_query() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("guest");
        let store = LibraryStore::new(profile.join("library.db"));
        let result: Option<()> = store
            .read(|_| panic!("missing library must not run query"))
            .unwrap();
        assert!(result.is_none());
        assert!(!profile.exists());
    }

    #[test]
    fn profiles_keep_identical_ids_and_mutations_separate() {
        let tmp = tempfile::tempdir().unwrap();
        let desktop = LibraryStore::new(tmp.path().join("desktop/users/0/library.db"));
        let daemon = LibraryStore::new(tmp.path().join("daemon/library.db"));
        desktop
            .write(|db| db.set_playlist_favorite(42, true))
            .unwrap();
        daemon
            .write(|db| db.set_playlist_favorite(7, true))
            .unwrap();
        assert_eq!(
            desktop.read(|db| db.get_favorite_playlist_ids()).unwrap(),
            Some(vec![42])
        );
        assert_eq!(
            daemon.read(|db| db.get_favorite_playlist_ids()).unwrap(),
            Some(vec![7])
        );
        let queued_operation = desktop.clone();
        daemon
            .write(|db| db.set_playlist_favorite(42, true))
            .unwrap();
        queued_operation
            .write(|db| db.set_playlist_favorite(42, false))
            .unwrap();
        assert!(desktop
            .read(|db| db.get_favorite_playlist_ids())
            .unwrap()
            .unwrap()
            .is_empty());
        let mut daemon_ids = daemon
            .read(|db| db.get_favorite_playlist_ids())
            .unwrap()
            .unwrap();
        daemon_ids.sort();
        assert_eq!(daemon_ids, vec![7, 42]);
    }

    #[test]
    fn failed_operation_is_not_reported_as_empty_library() {
        let tmp = tempfile::tempdir().unwrap();
        let store = LibraryStore::new(tmp.path().join("library.db"));
        store.open_or_create().unwrap();
        let result = store.read::<()>(|_| Err(LibraryError::Other("query failed".into())));
        assert!(matches!(result, Err(LibraryError::Other(_))));
        let invalid = tmp.path().join("file");
        std::fs::write(&invalid, b"not a directory").unwrap();
        assert!(LibraryStore::new(invalid.join("library.db"))
            .open_or_create()
            .is_err());
    }
}
