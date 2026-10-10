//! Explicit bounded caches: redb otherwise defaults to 1 GiB per database.
use redb::{Database, DatabaseError};
use std::path::Path;
pub const CACHE_BYTES: usize = 4 * 1024 * 1024;
pub fn create(path: impl AsRef<Path>) -> Result<Database, DatabaseError> {
    Database::builder().set_cache_size(CACHE_BYTES).create(path)
}
pub fn open(path: impl AsRef<Path>) -> Result<Database, DatabaseError> {
    Database::builder().set_cache_size(CACHE_BYTES).open(path)
}
