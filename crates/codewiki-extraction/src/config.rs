/// Maximum file size in bytes; files larger than this are skipped.
pub const MAX_FILE_SIZE_BYTES: u64 = 1_048_576; // 1 MB

/// Bounded channel depth between tokio I/O and rayon parse pool.
pub const EXTRACTION_CHANNEL_DEPTH: usize = 64;

/// If an edit touches more than this many lines, force a full reparse.
pub const INCREMENTAL_LOC_THRESHOLD: usize = 100;
