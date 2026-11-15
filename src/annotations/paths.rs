use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::{random_hex, Error};

// Every flag comes from libc: the values differ per architecture, and a wrong
// `O_NOFOLLOW` would silently let a confined write follow a symlink.
const DIRECTORY_MODE: libc::mode_t = 0o777;
const FILE_MODE: libc::c_uint = 0o600;

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

fn directory(parent: &File, entry: &OsStr, create: bool) -> io::Result<Option<File>> {
    match open_at(parent, entry, libc::O_DIRECTORY) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !create {
                return Ok(None);
            }
            let entry_name = name(entry)?;
            match check(unsafe {
                libc::mkdirat(parent.as_raw_fd(), entry_name.as_ptr(), DIRECTORY_MODE)
            }) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(path_error(
                        "create directory",
                        &entry.to_string_lossy(),
                        error,
                    ))
                }
            }
            open_at(parent, entry, libc::O_DIRECTORY).map(Some)
        }
        Err(error) => Err(error),
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
        if let Some(markview) = directory(&dir, OsStr::new(".markview"), false)? {
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
        for part in &parents {
            let Some(child) = directory(&dir, part, create)? else {
                return Ok(None);
            };
            dir = child;
        }
        Ok(Some(Parent {
            dir,
            filename: self
                .relative
                .file_name()
                .expect("store filename")
                .to_owned(),
        }))
    }
}

pub(super) struct Parent {
    dir: File,
    filename: OsString,
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
        let mut filename = self.filename.clone();
        filename.push(".markview-lock");
        let file = match open_at(&self.dir, &filename, libc::O_RDWR) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match open_at(
                    &self.dir,
                    &filename,
                    libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                ) {
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        open_at(&self.dir, &filename, libc::O_RDWR)
                    }
                    result => result,
                }
            }
            result => result,
        }?;
        let file = regular(file)?;
        loop {
            match check(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => return result.map(|()| file),
            }
        }
    }

    pub fn read(&self) -> io::Result<Option<Vec<u8>>> {
        let mut file = match open_at(&self.dir, &self.filename, libc::O_RDONLY) {
            Ok(file) => regular(file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
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
    let parent = canonical_directory(location.document.parent().expect("source parent"))?;
    let mut file = regular(open_at(
        &parent,
        location.document.file_name().expect("source filename"),
        libc::O_RDONLY,
    )?)?;
    let mut source = String::new();
    file.read_to_string(&mut source)?;
    Ok(source)
}
