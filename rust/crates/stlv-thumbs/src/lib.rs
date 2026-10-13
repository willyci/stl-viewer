//! Thumbnail cache and background thumbnail generation for the folder grid.

pub mod cache;
pub mod service;
mod sqlite;
#[cfg(test)]
mod test_util;

pub use cache::{
    CacheError, ThumbnailCache, cache_path_for, default_cache_path, path_hash, python_mtime,
};
pub use service::{
    LARGE_FILE_SIZE, MAX_RENDER_FILE_SIZE, THUMB_DISPLAY_SIZE, ThumbEvent, ThumbnailService,
};
