use std::{
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    io,
    mem::{offset_of, size_of, size_of_val},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
    ptr::{self, null, null_mut},
    slice,
};

use windows::Win32::Globalization::{CompareStringOrdinal, CSTR_EQUAL};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            NtOpenFile, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT,
            FILE_SYNCHRONOUS_IO_NONALERT,
        },
    },
    Win32::{
        Foundation::{
            RtlNtStatusToDosError, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, ERROR_PATH_NOT_FOUND,
            OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, STATUS_FILE_IS_A_DIRECTORY,
            STATUS_IO_REPARSE_TAG_NOT_HANDLED, STATUS_NOT_A_DIRECTORY, STATUS_OBJECT_NAME_INVALID,
            STATUS_OBJECT_NAME_NOT_FOUND, STATUS_OBJECT_PATH_NOT_FOUND,
            STATUS_REPARSE_POINT_ENCOUNTERED, STATUS_STOPPED_ON_SYMLINK, UNICODE_STRING,
        },
        Storage::FileSystem::{
            FileFullDirectoryInfo, GetFileInformationByHandleEx, GetFileType,
            GetFinalPathNameByHandleW, QueryDosDeviceW, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_FULL_DIR_INFO, FILE_GENERIC_READ, FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            FILE_TRAVERSE, FILE_TYPE_DISK, FILE_TYPE_UNKNOWN, SYNCHRONIZE, VOLUME_NAME_DOS,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

use super::{local_absolute_path, Kind, LocalState, Opened};

const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const DIRECTORY_ACCESS: u32 = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

pub(super) fn open(path: &Path, kind: Kind) -> io::Result<LocalState<Opened>> {
    let mut parts = path.components().peekable();
    let Some(Component::Prefix(prefix)) = parts.next() else {
        return Ok(LocalState::Rejected);
    };
    let Prefix::VerbatimDisk(drive) = prefix.kind() else {
        return Ok(LocalState::Rejected);
    };
    if parts.next() != Some(Component::RootDir) || (parts.peek().is_none() && kind == Kind::File) {
        return Ok(LocalState::Rejected);
    }
    let Some(root) = local_volume_root(drive)? else {
        return Ok(LocalState::Rejected);
    };
    let mut file = OpenOptions::new()
        .access_mode(
            DIRECTORY_ACCESS
                | if parts.peek().is_none() {
                    FILE_LIST_DIRECTORY
                } else {
                    0
                },
        )
        .share_mode(SHARE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(root)?;
    if !matches_kind(&file, Kind::Directory)? {
        return Ok(LocalState::Rejected);
    }

    let mut wide_name = Vec::new();
    while let Some(part) = parts.next() {
        let Component::Normal(name) = part else {
            return Ok(LocalState::Rejected);
        };
        let final_part = parts.peek().is_none();
        let expected_kind = if final_part { kind } else { Kind::Directory };
        wide_name.clear();
        wide_name.extend(name.encode_wide());
        file = match open_child(&file, &mut wide_name, expected_kind, final_part)? {
            LocalState::Present(child) => child,
            LocalState::Missing => return Ok(LocalState::Missing),
            LocalState::Rejected => return Ok(LocalState::Rejected),
        };
        if !matches_kind(&file, expected_kind)? {
            return Ok(LocalState::Rejected);
        }
    }

    // The independent expected path was screened before opening anything. Only
    // accept the handle's normalized DOS path when it agrees with that path, not
    // as a new authorization for whatever destination resolution returned.
    let Some(actual) = matching_final_path(&file, path)? else {
        return Ok(LocalState::Rejected);
    };
    Ok(LocalState::Present(Opened { file, path: actual }))
}

fn open_child(
    parent: &File,
    name: &mut [u16],
    kind: Kind,
    final_part: bool,
) -> io::Result<LocalState<File>> {
    let Ok(length) = u16::try_from(size_of_val(name)) else {
        return Ok(LocalState::Rejected);
    };
    let name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: name.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        SecurityDescriptor: null(),
        SecurityQualityOfService: null(),
    };
    let (access, options) = match kind {
        Kind::Directory => (
            DIRECTORY_ACCESS | if final_part { FILE_LIST_DIRECTORY } else { 0 },
            FILE_DIRECTORY_FILE,
        ),
        Kind::File => (FILE_GENERIC_READ, FILE_NON_DIRECTORY_FILE),
    };
    let mut status_block = IO_STATUS_BLOCK::default();
    let mut handle = null_mut();
    // SAFETY: the parent is an open directory; attributes and its single, counted
    // basename stay live throughout this synchronous call. It initializes handle
    // on success; status_block is writable for the duration of the call.
    let status = unsafe {
        NtOpenFile(
            &mut handle,
            access,
            &attributes,
            &mut status_block,
            SHARE,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        )
    };
    match status {
        STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND => Ok(LocalState::Missing),
        STATUS_REPARSE_POINT_ENCOUNTERED
        | STATUS_IO_REPARSE_TAG_NOT_HANDLED
        | STATUS_STOPPED_ON_SYMLINK
        | STATUS_OBJECT_NAME_INVALID
        | STATUS_FILE_IS_A_DIRECTORY
        | STATUS_NOT_A_DIRECTORY => Ok(LocalState::Rejected),
        status if status >= 0 => {
            // SAFETY: NtOpenFile succeeded and returned a newly owned file handle.
            Ok(LocalState::Present(unsafe {
                File::from_raw_handle(handle)
            }))
        }
        status => {
            // SAFETY: RtlNtStatusToDosError accepts an NTSTATUS value.
            Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(status) } as i32,
            ))
        }
    }
}

fn matches_kind(file: &File, kind: Kind) -> io::Result<bool> {
    // SAFETY: file owns a valid handle for the lifetime of this call.
    let file_type = unsafe { GetFileType(file.as_raw_handle()) };
    if file_type == FILE_TYPE_UNKNOWN {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(0) {
            return Err(error);
        }
    }
    if file_type != FILE_TYPE_DISK {
        return Ok(false);
    }
    let attributes = file.metadata()?.file_attributes();
    Ok(attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
        && (attributes & FILE_ATTRIBUTE_DIRECTORY != 0) == (kind == Kind::Directory))
}

fn matching_final_path(file: &File, path: &Path) -> io::Result<Option<PathBuf>> {
    let mut buffer = [0; 512];
    let mut len = final_path(file, &mut buffer)?;
    let mut large;
    let actual = if len < buffer.len() {
        &buffer[..len]
    } else {
        large = vec![0; len];
        len = final_path(file, &mut large)?;
        large
            .get(..len)
            .filter(|_| len < large.len())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "OMP handle path changed during lookup",
                )
            })?
    };
    if path.as_os_str().encode_wide().eq(actual.iter().copied()) {
        return Ok(Some(path.to_path_buf()));
    }
    let expected: Vec<_> = path.as_os_str().encode_wide().collect();
    // SAFETY: CompareStringOrdinal accepts counted UTF-16 slices.
    if unsafe { CompareStringOrdinal(actual, &expected, true) } != CSTR_EQUAL {
        return Ok(None);
    }
    local_absolute_path(Path::new(&OsString::from_wide(actual)))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid OMP handle path"))
        .map(Some)
}

fn final_path(file: &File, buffer: &mut [u16]) -> io::Result<usize> {
    // SAFETY: file owns the handle; the buffer is writable for its supplied length.
    let len = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if len == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(len as usize)
    }
}

fn local_volume_root(drive: u8) -> io::Result<Option<OsString>> {
    let name = [u16::from(drive), u16::from(b':'), 0];
    let mut targets = [0; 1024];
    // SAFETY: name is NUL-terminated and targets is writable for its supplied length.
    let len = unsafe { QueryDosDeviceW(name.as_ptr(), targets.as_mut_ptr(), targets.len() as u32) };
    if len == 0 {
        let error = io::Error::last_os_error();
        return match error.raw_os_error().map(|code| code as u32) {
            Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => Ok(None),
            _ => Err(error),
        };
    }
    let target = targets[..len as usize]
        .split(|&ch| ch == 0)
        .next()
        .unwrap_or_default();
    // Only allow a direct volume, never SUBST, UNC, or an arbitrary DOS device.
    // Use the checked device itself for the first open so alias changes cannot
    // route that open through a different (possibly network) drive mapping.
    let local = [
        r"\Device\HarddiskVolume",
        r"\Device\Floppy",
        r"\Device\CdRom",
        r"\Device\Ramdisk",
    ]
    .iter()
    .any(|prefix| {
        target.get(prefix.len()..).is_some_and(|suffix| {
            prefix
                .bytes()
                .map(u16::from)
                .eq(target[..prefix.len()].iter().copied())
                && suffix
                    .iter()
                    .all(|&ch| (u16::from(b'0')..=u16::from(b'9')).contains(&ch))
                && (!suffix.is_empty() || *prefix == r"\Device\Ramdisk")
        })
    });
    Ok(local.then(|| {
        let mut root = OsString::from(r"\\?\GLOBALROOT");
        root.push(OsString::from_wide(target));
        root.push("\\");
        root
    }))
}

pub(super) fn read_directory(
    file: &File,
    mut visit: impl FnMut(&OsStr, Kind) -> io::Result<()>,
) -> io::Result<()> {
    // FILE_FULL_DIR_INFO and its chained records require 8-byte alignment.
    let mut buffer = [0_u64; 1024];
    loop {
        // SAFETY: the live handle is a verified directory, and buffer is aligned
        // and writable for the supplied byte count.
        let success = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileFullDirectoryInfo,
                buffer.as_mut_ptr().cast(),
                size_of_val(&buffer) as u32,
            )
        };
        if success == 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                Ok(())
            } else {
                Err(error)
            };
        }
        // SAFETY: the initialized buffer is not mutated while the slice is used.
        let mut records =
            unsafe { slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), size_of_val(&buffer)) };
        loop {
            let malformed =
                || io::Error::new(io::ErrorKind::InvalidData, "invalid OMP directory record");
            if records.len() < size_of::<FILE_FULL_DIR_INFO>() {
                return Err(malformed());
            }
            // SAFETY: the record prefix is within initialized buffer storage and
            // all FILE_FULL_DIR_INFO fields are integers with no invalid bit pattern.
            let record =
                unsafe { ptr::read_unaligned(records.as_ptr().cast::<FILE_FULL_DIR_INFO>()) };
            let next = record.NextEntryOffset as usize;
            let offset = offset_of!(FILE_FULL_DIR_INFO, FileName);
            let name_end = offset
                .checked_add(record.FileNameLength as usize)
                .ok_or_else(malformed)?;
            let name = records
                .get(offset..name_end)
                .filter(|bytes| !bytes.is_empty() && bytes.len() % 2 == 0)
                .ok_or_else(malformed)?;
            if next != 0 && (next < name_end || !next.is_multiple_of(8) || next >= records.len()) {
                return Err(malformed());
            }
            if record.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                // SAFETY: the record starts at an 8-byte boundary; FileName has
                // an even offset and name is a validated even-length range.
                let wide =
                    unsafe { slice::from_raw_parts(name.as_ptr().cast::<u16>(), name.len() / 2) };
                let kind = if record.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                    Kind::File
                } else {
                    Kind::Directory
                };
                visit(&OsString::from_wide(wide), kind)?;
            }
            if next == 0 {
                break;
            }
            records = &records[next..];
        }
    }
}
