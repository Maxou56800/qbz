//! QBZ Library - Local music library management
//!
//! Provides functionality for scanning, indexing, and managing local audio files.
//! This crate is completely independent of Tauri and the Qobuz streaming functionality.
//!
//! ## Features
//!
//! - **Scanner**: Recursive directory scanning for audio files
//! - **Metadata**: Audio metadata extraction using lofty
//! - **Database**: SQLite persistence for library data
//! - **CUE Parser**: Support for CUE sheet single-file albums
//! - **Thumbnails**: Artwork extraction and thumbnail generation
//!
//! ## Usage
//!
//! ```no_run
//! use qbz_library::{LibraryScanner, MetadataExtractor, LibraryDatabase};
//! use std::path::Path;
//!
//! // Scan a directory for audio files
//! let scanner = LibraryScanner::new();
//! let result = scanner.scan_directory(Path::new("/path/to/music")).unwrap();
//!
//! // Extract metadata from a file
//! let track = MetadataExtractor::extract(&result.audio_files[0]).unwrap();
//!
//! // Open library database
//! let db = LibraryDatabase::open(Path::new("library.db")).unwrap();
//! ```

pub mod album_grouping;
mod cue_parser;
mod database;
pub mod ephemeral;
mod errors;
mod folder_registration;
pub mod local_playlists;
mod metadata;
mod models;
mod mount_info;
pub mod playlist_membership;
mod purchase_copies;
pub mod qobuz_playlist_snapshot;
pub mod reachability;
mod remote_tag_sidecar;
mod sacd;
pub mod sacd_scan;
mod scan;
mod scan_changes;
mod scanner;
pub mod search;
pub mod service;
mod store;
mod tag_sidecar;
mod tag_writer;
mod thumbnails;
mod watcher;

// Re-exports
pub use cue_parser::{cue_to_tracks, CueParser, CueSheet, CueTime, CueTrack};
pub use database::{
    AlbumTrackUpdate, LibraryDatabase, LibraryFolder, LibraryStats, LocalContentStatus,
    PlaylistFolder, PlaylistSettings, PlaylistStats, TrackMetadataUpdateFull,
};
pub use errors::LibraryError;
pub use folder_registration::RegisterFolderOutcome;
pub use metadata::MetadataExtractor;
pub use models::*;
pub use mount_info::{is_network_path, network_fs_label};
pub use purchase_copies::{
    PurchaseAlbumPlaybackPreference, PurchaseCopyHealth, PurchaseCopySnapshot,
    PurchaseDownloadCopy, PurchaseDownloadCopyTrack, PurchasePlaybackMode, PurchaseTrackHealth,
};
pub use reachability::{
    probe, probe_default, probe_file, probe_file_default, FileProbe, Reach, DEFAULT_PROBE,
};
pub use sacd::{SacdImageImport, SacdImportResult};
pub use sacd_scan::{
    build_image_rows, SacdImageRows, SacdLabels, SacdScanSummary, SACD_PARSER_REVISION,
};
pub use scan::{scan_with_progress, ScanEvent};
pub use scanner::{
    LibraryScanner, ScanEntry, ScanFileKind, ScanResult, ScanStream, ScanWalkError, SymlinkPolicy,
};
pub use store::LibraryStore;
pub use tag_writer::{
    compute_track_artist_match, inspect_album_tag_layers, read_editor_tag_snapshots,
    write_album_tags_to_files, write_album_tags_to_files_extended,
    write_album_tags_to_files_with_options, write_folder_front_cover, write_purchase_tags,
    write_sacd_front_cover, AlbumTagInspection, AlbumTagWrite, DirectTagWriteOptions,
    EditorTrackTagSnapshot, ExtendedAlbumTagWrite, ExtendedTrackTagWrite, FrontCoverWrite,
    Id3v2WriteVersion, PurchaseTagWrite, TagLayerInspection, TrackTagWrite,
};
pub use thumbnails::{
    clear_thumbnails, generate_thumbnail, generate_thumbnail_from_bytes, get_cache_size,
    get_or_generate_large_thumbnail, get_or_generate_large_thumbnail_from_bytes,
    get_or_generate_thumbnail, get_thumbnail_path, get_thumbnails_dir, thumbnail_exists,
    LARGE_ART_PX,
};
pub use watcher::{LocalRootWatcher, RootWatchEvent};

// Re-export database module for backwards compatibility
pub mod database_exports {
    pub use crate::database::*;
}
pub use remote_tag_sidecar::*;
pub use tag_sidecar::*;

use std::path::PathBuf;

/// Get library database path in app data directory
pub fn get_db_path() -> PathBuf {
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qbz");
    std::fs::create_dir_all(&data_dir).ok();
    data_dir.join("library.db")
}

/// Get artwork cache directory
pub fn get_artwork_cache_dir() -> PathBuf {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qbz")
        .join("artwork");
    std::fs::create_dir_all(&cache_dir).ok();
    cache_dir
}
