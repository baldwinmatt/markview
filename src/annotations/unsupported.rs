//! Store paths for platforms without POSIX `*at` file operations.
//!
//! Confined writes need `openat`/`renameat` with `O_NOFOLLOW`. Where those do
//! not exist every store operation reports one clear error, so the rest of the
//! crate still builds and renders.
// Unix builds compile this file only in tests, to keep it type-checked.
#![cfg_attr(unix, allow(dead_code))]

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::Error;

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "annotations are unsupported on this platform",
    )
}

#[derive(Debug)]
pub struct Location {
    pub document: PathBuf,
    pub root: PathBuf,
    pub path: PathBuf,
    pub(super) relative: PathBuf,
}

pub fn resolve(_document: &Path) -> io::Result<Location> {
    Err(unsupported())
}

impl Location {
    pub(super) fn parent(&self, _create: bool) -> io::Result<Option<Parent>> {
        Err(unsupported())
    }
}

/// Never constructed: `Location::parent` always fails on these platforms.
pub(super) enum Parent {}

impl Parent {
    pub fn lock(&self) -> io::Result<File> {
        match *self {}
    }

    pub fn discard(&self, _lock: Option<&File>) {
        match *self {}
    }

    pub fn read(&self) -> io::Result<Option<Vec<u8>>> {
        match *self {}
    }

    pub fn commit(&self, _snapshot: Option<&[u8]>, _content: Option<&str>) -> Result<(), Error> {
        match *self {}
    }
}

pub fn read_source(_location: &Location) -> io::Result<String> {
    Err(unsupported())
}

pub fn read_source_limited(_location: &Location, _limit: u64) -> io::Result<String> {
    Err(unsupported())
}

/// Reads the file without the symlink confinement unix has: this platform
/// cannot write annotations, but it can still serve documents.
pub fn read_canonical_source(path: &Path) -> io::Result<String> {
    read_canonical_source_limited(path, u64::MAX)
}

pub fn read_canonical_source_limited(path: &Path, limit: u64) -> io::Result<String> {
    use std::io::Read;
    let too_large = || io::Error::new(io::ErrorKind::FileTooLarge, "document is too large");
    let file = File::open(path)?;
    if file.metadata()?.len() > limit {
        return Err(too_large());
    }
    let mut source = String::new();
    file.take(limit.saturating_add(1))
        .read_to_string(&mut source)?;
    if source.len() as u64 > limit {
        return Err(too_large());
    }
    Ok(source)
}
