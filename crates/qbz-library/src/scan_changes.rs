//! Connection-local change tracking for a scan. Scan checkpoints, generations
//! and last-scan timestamps are not changes to the browsable catalog.
//!
//! TEMP triggers observe committed track writes, including CUE/SACD imports and
//! partial scans. The flag rolls back with a failed batch. No persistent schema
//! changes, full-library copies or hashes are needed for an unchanged scan.

use rusqlite::Connection;

pub(crate) fn begin(connection: &Connection) -> rusqlite::Result<()> {
    let mut columns = connection.prepare("PRAGMA main.table_info(local_tracks)")?;
    let changed_columns = columns
        .query_map([], |row| row.get::<_, String>(1))?
        .map(|column| {
            column.map(|name| {
                let quoted = name.replace('"', "\"\"");
                format!("OLD.\"{quoted}\" IS NOT NEW.\"{quoted}\"")
            })
        })
        .collect::<rusqlite::Result<Vec<_>>>()?
        .join(" OR ");
    connection.execute_batch(&format!(
        "CREATE TEMP TABLE qbz_scan_changes (changed INTEGER NOT NULL);
         INSERT INTO qbz_scan_changes VALUES (0);
         CREATE TEMP TRIGGER qbz_scan_insert AFTER INSERT ON main.local_tracks
         BEGIN UPDATE qbz_scan_changes SET changed=1 WHERE changed=0; END;
         CREATE TEMP TRIGGER qbz_scan_delete AFTER DELETE ON main.local_tracks
         BEGIN UPDATE qbz_scan_changes SET changed=1 WHERE changed=0; END;
         CREATE TEMP TRIGGER qbz_scan_update AFTER UPDATE ON main.local_tracks
         WHEN {changed_columns}
         BEGIN UPDATE qbz_scan_changes SET changed=1 WHERE changed=0; END;"
    ))
}

pub(crate) fn changed(connection: &Connection) -> rusqlite::Result<bool> {
    connection.query_row("SELECT changed FROM temp.qbz_scan_changes", [], |row| row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookkeeping_and_rolled_back_batches_do_not_invalidate_content() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE local_tracks(id INTEGER PRIMARY KEY, title TEXT, artwork_path TEXT);
             CREATE TABLE local_scan_roots(generation INTEGER);
             INSERT INTO local_tracks VALUES(1,'Dogs',NULL);"
        ).unwrap();
        begin(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO local_scan_roots VALUES(104);
             UPDATE local_scan_roots SET generation=105;
             UPDATE local_tracks SET title=title, artwork_path=NULL;
             BEGIN; DELETE FROM local_tracks; ROLLBACK;"
        ).unwrap();
        assert!(!changed(&conn).unwrap());
        conn.execute_batch("UPDATE local_tracks SET artwork_path='cover.jpg'").unwrap();
        assert!(changed(&conn).unwrap());
    }

    #[test]
    fn inserts_deletes_and_committed_partial_imports_invalidate_content() {
        for mutation in [
            "INSERT INTO local_tracks VALUES(2,'Sheep')",
            "DELETE FROM local_tracks",
            "UPDATE local_tracks SET title='Dogs (Remix)'",
        ] {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE local_tracks(id INTEGER PRIMARY KEY, title TEXT);
                 INSERT INTO local_tracks VALUES(1,'Dogs');"
            ).unwrap();
            begin(&conn).unwrap();
            conn.execute_batch(mutation).unwrap();
            conn.execute_batch("BEGIN; DELETE FROM local_tracks; ROLLBACK;").unwrap();
            assert!(changed(&conn).unwrap(), "{mutation}");
        }
    }
}
