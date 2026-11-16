use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::{random_hex, revision, Error};

// Every flag comes from libc: the values differ per architecture, and a wrong
// `O_NOFOLLOW` would silently let a confined write follow a symlink.
const DIRECTORY_MODE: libc::mode_t = 0o777;
// Store, lock and temporary files are created 0666 so the umask decides.
const FILE_MODE: libc::c_uint = 0o666;

fn name(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid path component"))
}

fn open_at(parent: &File, entry: &OsStr, flags: libc::c_int) -> io::Result<File> {
    let entry = name(entry)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            entry.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            FILE_MODE,
        )
    };
    if fd < 0 {
        Err(path_error(
            "open",
            &entry.to_string_lossy(),
            io::Error::last_os_error(),
        ))
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn path_error(action: &str, name: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{action} {name}: {error}"))
}

fn check(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Opens `entry` under `parent`, optionally creating it. The flag reports
/// whether this call made the directory, so a failed first write can undo it.
fn directory(parent: &File, entry: &OsStr, create: bool) -> io::Result<Option<(File, bool)>> {
    match open_at(parent, entry, libc::O_DIRECTORY) {
        Ok(file) => Ok(Some((file, false))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !create {
                return Ok(None);
            }
            let entry_name = name(entry)?;
            let created = match check(unsafe {
                libc::mkdirat(parent.as_raw_fd(), entry_name.as_ptr(), DIRECTORY_MODE)
            }) {
                Ok(()) => true,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
                Err(error) => {
                    return Err(path_error(
                        "create directory",
                        &entry.to_string_lossy(),
                        error,
                    ))
                }
            };
            open_at(parent, entry, libc::O_DIRECTORY).map(|file| Some((file, created)))
        }
        Err(error) => Err(error),
    }
}

/// Removes directories this process created, deepest first. A directory that
/// has gained content since is not empty, so removal stops there.
fn remove_directories(created: &[(File, CString)]) {
    for (parent, entry) in created.iter().rev() {
        let removed =
            unsafe { libc::unlinkat(parent.as_raw_fd(), entry.as_ptr(), libc::AT_REMOVEDIR) };
        if removed != 0 {
            break;
        }
    }
}

fn canonical_directory(path: &Path) -> io::Result<File> {
    let mut dir = File::open("/")?;
    for part in path
        .strip_prefix("/")
        .expect("absolute canonical directory")
        .iter()
    {
        dir = open_at(&dir, part, libc::O_DIRECTORY)?;
    }
    Ok(dir)
}

#[derive(Debug)]
pub struct Location {
    pub document: PathBuf,
    pub root: PathBuf,
    pub path: PathBuf,
    pub(super) relative: PathBuf,
}

pub fn resolve(document: &Path) -> io::Result<Location> {
    let document = document.canonicalize()?;
    if !document.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular Markdown document",
        ));
    }
    let parent = document.parent().expect("document parent");
    let mut git = None;
    let mut store = None;
    for ancestor in parent.ancestors() {
        let dir = canonical_directory(ancestor)?;
        if let Some((markview, _)) = directory(&dir, OsStr::new(".markview"), false)? {
            if directory(&markview, OsStr::new("annotations"), false)?.is_some() {
                store = Some(ancestor);
                break;
            }
        }
        if git.is_none() && fs::symlink_metadata(ancestor.join(".git")).is_ok() {
            git = Some(ancestor);
        }
    }
    let root = store.or(git).unwrap_or(parent).to_owned();
    let relative = document
        .strip_prefix(&root)
        .expect("ancestor root")
        .to_owned();
    let path = root.join(".markview/annotations").join(&relative);
    Ok(Location {
        document,
        root,
        path,
        relative,
    })
}

impl Location {
    pub(super) fn parent(&self, create: bool) -> io::Result<Option<Parent>> {
        let mut dir = canonical_directory(&self.root)?;
        let parents = Path::new(".markview/annotations")
            .join(self.relative.parent().unwrap_or(Path::new("")));
        let mut created = Vec::new();
        for part in &parents {
            let (child, made) = match directory(&dir, part, create) {
                Ok(Some(child)) => child,
                Ok(None) => return Ok(None),
                Err(error) => {
                    remove_directories(&created);
                    return Err(error);
                }
            };
            let previous = std::mem::replace(&mut dir, child);
            if made {
                created.push((previous, name(part)?));
            }
        }
        Ok(Some(Parent {
            dir,
            filename: self
                .relative
                .file_name()
                .expect("store filename")
                .to_owned(),
            created,
        }))
    }
}

pub(super) struct Parent {
    dir: File,
    filename: OsString,
    /// Directories made by `Location::parent(true)`: the containing directory
    /// handle and the new entry's name, outermost first.
    created: Vec<(File, CString)>,
}

const LOCK_SUFFIX: &str = ".markview-lock";
// The longest file name, in bytes, that the supported filesystems accept.
const NAME_MAX: usize = 255;

/// One lock per document, next to its store: `<document name>.markview-lock`.
/// A name too long to take the suffix locks on a fixed-length digest of the
/// name instead, so every document that can be read can also be written.
fn lock_name(document: &OsStr) -> OsString {
    let mut name = if document.len() + LOCK_SUFFIX.len() <= NAME_MAX {
        document.to_owned()
    } else {
        OsString::from(revision(&[document.as_bytes()]))
    };
    name.push(LOCK_SUFFIX);
    name
}

fn regular(file: File) -> io::Result<File> {
    if file.metadata()?.is_file() {
        Ok(file)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "annotation entry must be a regular file",
        ))
    }
}

impl Parent {
    pub fn lock(&self) -> io::Result<File> {
        use std::os::unix::fs::MetadataExt;
        let filename = lock_name(&self.filename);
        loop {
            let file = match open_at(&self.dir, &filename, libc::O_RDWR) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match open_at(
                        &self.dir,
                        &filename,
                        libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                    ) {
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                        result => result,
                    }
                }
                result => result,
            }?;
            let file = regular(file)?;
            loop {
                match check(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    result => break result,
                }
            }?;
            // `discard` unlinks the lock after a failed first write. If that
            // happened while this call waited, the file locked here no longer
            // guards the store: take the one now at that name instead.
            let held = file.metadata()?;
            match open_at(&self.dir, &filename, libc::O_RDONLY).and_then(|file| file.metadata()) {
                Ok(current) if current.dev() == held.dev() && current.ino() == held.ino() => {
                    return Ok(file)
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// Undoes a first write that failed, so it leaves no empty store
    /// directories behind. Only directories this handle created are removed.
    /// Pass the held lock to remove its file as well; without it the lock may
    /// belong to another writer and stays.
    pub fn discard(&self, lock: Option<&File>) {
        if self.created.is_empty() {
            return;
        }
        if lock.is_some() && matches!(self.open_store(), Ok(None)) {
            if let Ok(filename) = name(&lock_name(&self.filename)) {
                let _ = unsafe { libc::unlinkat(self.dir.as_raw_fd(), filename.as_ptr(), 0) };
            }
        }
        remove_directories(&self.created);
    }

    fn open_store(&self) -> io::Result<Option<File>> {
        match open_at(&self.dir, &self.filename, libc::O_RDONLY) {
            Ok(file) => regular(file).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn read(&self) -> io::Result<Option<Vec<u8>>> {
        let Some(mut file) = self.open_store()? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    }

    fn unchanged(&self, snapshot: Option<&[u8]>) -> Result<(), Error> {
        if self.read()?.as_deref() != snapshot {
            return Err(Error::Conflict("annotation store changed on disk"));
        }
        Ok(())
    }

    pub fn commit(&self, snapshot: Option<&[u8]>, content: Option<&str>) -> Result<(), Error> {
        let filename = name(&self.filename)?;
        let fd = self.dir.as_raw_fd();
        let Some(content) = content else {
            self.unchanged(snapshot)?;
            return Ok(check(unsafe { libc::unlinkat(fd, filename.as_ptr(), 0) })?);
        };
        let temp = format!(".markview-{}.tmp", random_hex()?);
        let temp_name = name(OsStr::new(&temp))?;
        let mut file = open_at(
            &self.dir,
            OsStr::new(&temp),
            libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY,
        )?;
        let result = (|| {
            file.write_all(content.as_bytes())?;
            if snapshot.is_some() {
                // The rename replaces the inode, so carry the mode across.
                if let Some(existing) = self.open_store()? {
                    file.set_permissions(existing.metadata()?.permissions())?;
                }
            }
            file.sync_all()?;
            self.unchanged(snapshot)?;
            check(unsafe { libc::renameat(fd, temp_name.as_ptr(), fd, filename.as_ptr()) })
                .map_err(|error| path_error("replace", &self.filename.to_string_lossy(), error))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = unsafe { libc::unlinkat(fd, temp_name.as_ptr(), 0) };
        }
        result
    }
}

pub fn read_source(location: &Location) -> io::Result<String> {
    read_canonical_source(&location.document)
}

/// `read_source`, refusing a document larger than `limit` bytes.
pub fn read_source_limited(location: &Location, limit: u64) -> io::Result<String> {
    read_canonical_source_limited(&location.document, limit)
}

/// Reads a canonical document path without following symlinks in any component.
pub fn read_canonical_source(path: &Path) -> io::Result<String> {
    read_canonical_source_limited(path, u64::MAX)
}

/// Like `read_canonical_source`, but refuses a file larger than `limit` bytes
/// with `ErrorKind::FileTooLarge`: by its size before reading it, and by
/// bounding the read in case the file grows meanwhile.
pub fn read_canonical_source_limited(path: &Path, limit: u64) -> io::Result<String> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
    let parent = path
        .parent()
        .filter(|_| path.is_absolute())
        .ok_or_else(|| invalid("expected an absolute file path"))?;
    let filename = path
        .file_name()
        .ok_or_else(|| invalid("expected a file path"))?;
    let parent = canonical_directory(parent)?;
    let mut file = regular(open_at(&parent, filename, libc::O_RDONLY)?)?;
    let too_large = || io::Error::new(io::ErrorKind::FileTooLarge, "document is too large");
    if file.metadata()?.len() > limit {
        return Err(too_large());
    }
    let mut source = String::new();
    (&mut file)
        .take(limit.saturating_add(1))
        .read_to_string(&mut source)?;
    if source.len() as u64 > limit {
        return Err(too_large());
    }
    Ok(source)
}
