//! Reuses the Tauri-era updates.db and preserves existing opt-outs.
use crate::Result;
use rusqlite::Connection;
use std::path::Path;

pub struct Store(Connection);
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS update_preferences (id INTEGER PRIMARY KEY CHECK(id=1), check_on_launch INTEGER NOT NULL DEFAULT 1, show_whats_new_on_launch INTEGER NOT NULL DEFAULT 1);
            INSERT OR IGNORE INTO update_preferences (id,check_on_launch,show_whats_new_on_launch) VALUES (1,1,1);
            CREATE TABLE IF NOT EXISTS ignored_releases (version TEXT PRIMARY KEY, ignored_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS acknowledged_releases (version TEXT PRIMARY KEY, acknowledged_at INTEGER NOT NULL);")
            .map_err(|e| e.to_string())?;
        Ok(Self(conn))
    }
    pub fn check_on_launch(&self) -> Result<bool> {
        self.0
            .query_row(
                "SELECT check_on_launch FROM update_preferences WHERE id=1",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
    }
    pub fn set_check_on_launch(&self, enabled: bool) -> Result<()> {
        self.0
            .execute(
                "UPDATE update_preferences SET check_on_launch=? WHERE id=1",
                [enabled],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn suppressed(&self, version: &str) -> Result<bool> {
        self.0.query_row("SELECT EXISTS(SELECT 1 FROM ignored_releases WHERE version=?1 UNION ALL SELECT 1 FROM acknowledged_releases WHERE version=?1)", [version], |r| r.get(0)).map_err(|e| e.to_string())
    }
    pub fn ignore(&self, version: &str) -> Result<()> {
        self.0
            .execute(
                "INSERT OR REPLACE INTO ignored_releases VALUES (?1,unixepoch())",
                [version],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_existing_opt_out_and_ignores_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.db");
        let s = Store::open(&path).unwrap();
        assert!(s.check_on_launch().unwrap());
        s.set_check_on_launch(false).unwrap();
        s.ignore("2.1.2").unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        assert!(!s.check_on_launch().unwrap());
        assert!(s.suppressed("2.1.2").unwrap());
        assert!(!s.suppressed("2.1.3").unwrap());
    }
}
