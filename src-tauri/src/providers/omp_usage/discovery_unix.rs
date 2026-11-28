use std::{
    ffi::OsStr,
    fs::File,
    io,
    os::unix::ffi::OsStrExt,
    path::{Component, Path},
};

use rustix::{
    fs::{self, AtFlags, Dir, FileType, Mode, OFlags},
    io::Errno,
};

use super::{Kind, LocalState, Opened};

pub(super) fn open(path: &Path, kind: Kind) -> io::Result<LocalState<Opened>> {
    let mut parts = path.components().peekable();
    if parts.next() != Some(Component::RootDir) {
        return Ok(LocalState::Rejected);
    }
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut fd = fs::open("/", flags | OFlags::DIRECTORY, Mode::empty())?;
    if parts.peek().is_none() && kind == Kind::File {
        return Ok(LocalState::Rejected);
    }
    while let Some(component) = parts.next() {
        let Component::Normal(name) = component else {
            return Ok(LocalState::Rejected);
        };
        let directory = parts.peek().is_some() || kind == Kind::Directory;
        let flags = if directory {
            flags | OFlags::DIRECTORY
        } else {
            // Reject known special files before open. The checked open below
            // still needs NONBLOCK in case a regular file became a pipe.
            match fs::statat(&fd, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) if FileType::from_raw_mode(stat.st_mode).is_file() => {}
                Ok(_) => return Ok(LocalState::Rejected),
                Err(error) => return classify(error),
            }
            flags | OFlags::NONBLOCK | OFlags::NOCTTY
        };
        fd = match fs::openat(&fd, name, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(error) => return classify(error),
        };
    }
    let file = File::from(fd);
    if kind == Kind::File && !file.metadata()?.is_file() {
        return Ok(LocalState::Rejected);
    }
    Ok(LocalState::Present(Opened {
        file,
        path: path.to_path_buf(),
    }))
}

fn classify<T>(error: Errno) -> io::Result<LocalState<T>> {
    match error {
        Errno::NOENT => Ok(LocalState::Missing),
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => Ok(LocalState::Rejected),
        _ => Err(error.into()),
    }
}

pub(super) fn read_directory(
    file: &File,
    mut visit: impl FnMut(&OsStr, Kind) -> io::Result<()>,
) -> io::Result<()> {
    let mut entry_error = None;
    for entry in Dir::read_from(file)? {
        let entry = entry?;
        let name = entry.file_name();
        let file_type = match entry.file_type() {
            FileType::Unknown => match fs::statat(file, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => FileType::from_raw_mode(stat.st_mode),
                Err(error) => {
                    // This entry could not be classified; keep enumerating its siblings.
                    entry_error.get_or_insert(error);
                    continue;
                }
            },
            kind => kind,
        };
        let kind = match file_type {
            FileType::Directory => Kind::Directory,
            FileType::RegularFile => Kind::File,
            _ => continue,
        };
        visit(OsStr::from_bytes(name.to_bytes()), kind)?;
    }
    entry_error.map_or(Ok(()), |error| Err(error.into()))
}
