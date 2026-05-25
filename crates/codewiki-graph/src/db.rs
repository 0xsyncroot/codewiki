//! Read-only SQLite connection factory for the graph HTTP server.
//!
//! The graph server opens its own **read-only** `rusqlite::Connection` —
//! completely separate from `StorageImpl`'s writer `Mutex<Connection>`.
//! WAL mode allows this read-only connection to read a consistent snapshot
//! while the sync daemon continues appending to the WAL.

use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

pub type ReadDb = Arc<Mutex<Connection>>;

/// Open a read-only WAL connection suitable for the graph server.
pub fn open_readonly(path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // PRAGMAs safe on read-only connections:
    conn.execute_batch("PRAGMA busy_timeout = 3000;")?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    conn.execute_batch("PRAGMA cache_size = -32000;")?; // 32 MB
    conn.execute_batch("PRAGMA temp_store = MEMORY;")?;
    conn.execute_batch("PRAGMA mmap_size = 268435456;")?;
    Ok(conn)
}
