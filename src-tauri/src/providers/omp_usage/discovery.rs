use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek},
    path::{Component, Path, PathBuf},
};

#[cfg(unix)]
#[path = "discovery_unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "discovery_windows.rs"]
mod platform;

use serde::Deserialize;

const MAX_MARKER_BYTES: u64 = 128 * 1024;
const MAX_HEADER_BYTES: u64 = 128 * 1024;

/// A scan's path authorization and the lineage observed on opened journals.
#[derive(Debug)]
pub(super) struct DiscoveredFiles {
    paths: Vec<PathBuf>,
    headers: HashMap<PathBuf, Header>,
    home: PathBuf,
    input_home: Option<PathBuf>,
    discovery: DiscoveryReads,
}

impl DiscoveredFiles {
    pub(super) fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub(super) fn is_complete(&self) -> bool {
        !self.discovery.incomplete
    }

    /// Classify a legacy cache key by location, not by current filesystem safety.
    /// The opener must still confirm that a retained path is really missing.
    pub(super) fn accepts_legacy_path(&self, path: &Path) -> bool {
        local_absolute_path(path).is_some_and(|path| self.is_legacy_path(&path))
    }

    fn is_legacy_path(&self, path: &Path) -> bool {
        let relative = path
            .strip_prefix(&self.home)
            .ok()
            .or_else(|| path.strip_prefix(self.input_home.as_deref()?).ok());
        relative.is_some_and(|relative| {
            let mut parts = relative.components();
            ["agent", "sessions"].into_iter().all(|expected| {
                let Some(Component::Normal(name)) = parts.next() else {
                    return false;
                };
                #[cfg(windows)]
                return name.eq_ignore_ascii_case(OsStr::new(expected));
                #[cfg(not(windows))]
                return name == OsStr::new(expected);
            }) && parts.next().is_some()
                && is_journal_name(relative)
        })
    }

    /// Only a securely missing path returns None. An unsafe replacement must
    /// not be mistaken for a deleted journal that can be recovered from cache.
    /// This does not prevent writers from changing an ordinary file in place.
    pub(super) fn open(&self, path: &Path) -> io::Result<Option<File>> {
        let normalized;
        let (path, listed) =
            if let Ok(index) = self.paths.binary_search_by(|p| p.as_path().cmp(path)) {
                (self.paths[index].as_path(), true)
            } else {
                normalized = local_absolute_path(path).ok_or_else(rejected_path)?;
                (
                    normalized.as_path(),
                    self.paths.binary_search(&normalized).is_ok(),
                )
            };
        if !listed && !self.is_legacy_path(path) {
            return Err(rejected_path());
        }

        let opened = match platform::open(path, Kind::File)? {
            LocalState::Missing => return Ok(None),
            LocalState::Rejected => return Err(rejected_path()),
            LocalState::Present(opened) if listed && opened.path == path => opened,
            LocalState::Present(_) => return Err(rejected_path()),
        };
        let mut file = opened.file;
        if let Some(expected) = self.headers.get(path) {
            if read_header(&mut file)?.as_ref() != Some(expected) {
                return Err(rejected_path());
            }
            file.rewind()?;
        }
        Ok(Some(file))
    }
}

/// Collect the authorized path set for an already approved OMP home.
/// Unreadable entries and subtrees leave usable paths but make pruning unsafe.
pub(super) fn discover_omp_files(
    home: &Path,
    managed_roots: &[&Path],
) -> io::Result<DiscoveredFiles> {
    let home = local_absolute_path(home).ok_or_else(invalid_root)?;
    let mut files = DiscoveredFiles {
        paths: Vec::new(),
        headers: HashMap::new(),
        input_home: None,
        home,
        discovery: DiscoveryReads::default(),
    };
    let Some(canonical_home) = local_directory(&files.home)? else {
        return Ok(files);
    };
    if canonical_home != files.home {
        files.input_home = Some(std::mem::replace(&mut files.home, canonical_home));
    }
    let canonical_home = &files.home;

    let mut paths = HashSet::new();
    let discovery = &mut files.discovery;
    let default = discovery.directory(&canonical_home.join("agent/sessions"));
    if let Some(default) = &default {
        collect_default(discovery, default, &mut paths);
    }

    let mut companions = HashSet::new();
    for root in managed_roots {
        let root = local_absolute_path(root).ok_or_else(invalid_root)?;
        let relative = root
            .strip_prefix(canonical_home)
            .or_else(|_| root.strip_prefix(files.input_home.as_deref().unwrap_or(canonical_home)))
            .ok()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(invalid_root)?;
        if let Some(root) = discovery.directory(&canonical_home.join(relative)) {
            if !root.starts_with(canonical_home) {
                return Err(invalid_root());
            }
            collect_managed(
                discovery,
                root,
                default.as_deref(),
                &mut paths,
                &mut files.headers,
                &mut companions,
            );
        }
    }

    if let Some(registry) = discovery.directory(&canonical_home.join("agent/custom-session-files"))
    {
        collect_registered(
            discovery,
            &registry,
            &mut paths,
            &mut files.headers,
            &mut companions,
        );
    }

    files.paths = paths.into_iter().collect();
    files.paths.sort_unstable();
    Ok(files)
}

fn invalid_root() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid local OMP session root",
    )
}

fn rejected_path() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "OMP session file is not authorized",
    )
}

fn is_journal_name(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("jsonl"))
}

// The original managed sessions tree also contains legacy journals without explicit lineage.
fn collect_default(discovery: &mut DiscoveryReads, root: &Path, paths: &mut HashSet<PathBuf>) {
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        discovery.walk(&directory, |_, path, kind| {
            match kind {
                Kind::Directory => directories.push(path),
                Kind::File if is_journal_name(&path) => {
                    paths.insert(path);
                }
                Kind::File => {}
            }
            Ok(())
        });
    }
}

// An explicitly managed tree can contain ordinary journal directories, but a sibling
// directory named for a journal stem is an artifact directory, not a second managed root.
fn collect_managed(
    discovery: &mut DiscoveryReads,
    root: PathBuf,
    default: Option<&Path>,
    paths: &mut HashSet<PathBuf>,
    headers: &mut HashMap<PathBuf, Header>,
    companions: &mut HashSet<PathBuf>,
) {
    let mut directories = vec![root];
    let mut journals = Vec::new();
    while let Some(directory) = directories.pop() {
        if default.is_some_and(|default| directory.starts_with(default))
            || companions.contains(&directory)
        {
            continue;
        }
        discovery.walk(&directory, |_, path, kind| {
            if kind == Kind::Directory {
                directories.push(path);
            } else if is_journal_name(&path) && !paths.contains(&path) {
                if let Some(journal) = read_journal(&path)? {
                    journals.push(journal);
                }
            }
            Ok(())
        });
        collect_companions(discovery, &mut journals, paths, headers, companions);
    }
}

fn collect_registered(
    discovery: &mut DiscoveryReads,
    registry: &Path,
    paths: &mut HashSet<PathBuf>,
    headers: &mut HashMap<PathBuf, Header>,
    companions: &mut HashSet<PathBuf>,
) {
    let mut bytes = Vec::new();
    let mut journals = Vec::new();
    discovery.walk(registry, |discovery, marker, kind| {
        if kind != Kind::File {
            return Ok(());
        }
        let LocalFileState::Present(file) = local_file_state(&marker)? else {
            return Ok(());
        };
        if file.metadata()?.len() > MAX_MARKER_BYTES {
            return Ok(());
        }
        bytes.clear();
        file.take(MAX_MARKER_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MARKER_BYTES {
            return Ok(());
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return Ok(());
        };
        // Native markers have no terminator. Also accept a single line terminator,
        // without trimming any spaces that can belong to an actual file name.
        let text = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(text);
        let target = Path::new(text);
        // Screen the name before opening any registered target.
        if text.contains(['\0', '\r', '\n']) || !is_journal_name(target) {
            return Ok(());
        }
        let Some(path) = local_absolute_path(target) else {
            return Ok(());
        };
        if !paths.contains(&path) {
            if let Some(journal) = read_journal(&path)? {
                journals.push(journal);
                collect_companions(discovery, &mut journals, paths, headers, companions);
            }
        }
        Ok(())
    });
}

struct Journal {
    path: PathBuf,
    header: Header,
}

#[derive(Debug, PartialEq, Eq)]
struct Header {
    id: String,
    parent_session: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum FirstRecord {
    Session,
    Title,
}

#[derive(Deserialize)]
struct HeaderRecord {
    #[serde(rename = "type")]
    kind: FirstRecord,
    id: Option<String>,
    #[serde(rename = "parentSession")]
    parent_session: Option<String>,
}

fn read_journal(path: &Path) -> io::Result<Option<Journal>> {
    let Some(Opened { mut file, path }) = platform::open(path, Kind::File)?.present() else {
        return Ok(None);
    };
    Ok(read_header(&mut file)?.map(|header| Journal { path, header }))
}

fn read_header(file: &mut File) -> io::Result<Option<Header>> {
    let mut reader = BufReader::new(file.take(MAX_HEADER_BYTES + 1));
    let mut line = Vec::new();
    let mut read = 0;
    let mut title_seen = false;
    loop {
        line.clear();
        let len = reader.read_until(b'\n', &mut line)?;
        read += len as u64;
        if len == 0 || read > MAX_HEADER_BYTES {
            return Ok(None);
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<HeaderRecord>(&line) {
            Ok(HeaderRecord {
                kind: FirstRecord::Title,
                ..
            }) if !title_seen => title_seen = true,
            Ok(HeaderRecord {
                kind: FirstRecord::Session,
                id: Some(id),
                parent_session,
            }) if !id.trim().is_empty() => return Ok(Some(Header { id, parent_session })),
            _ => return Ok(None),
        }
    }
}

fn collect_companions(
    discovery: &mut DiscoveryReads,
    journals: &mut Vec<Journal>,
    paths: &mut HashSet<PathBuf>,
    headers: &mut HashMap<PathBuf, Header>,
    companions: &mut HashSet<PathBuf>,
) {
    while let Some(Journal { path, header }) = journals.pop() {
        if paths.contains(&path) {
            continue;
        }
        let stem = path.file_stem().expect("journal has a file name");
        if valid_component(stem) {
            let directory = path.with_extension("");
            if let Some(directory) = discovery.directory(&directory) {
                if !companions.contains(&directory) {
                    linked_journals(discovery, &directory, &path, &header.id, journals);
                    companions.insert(directory);
                }
            }
        }
        paths.insert(path.clone());
        headers.insert(path, header);
    }
}

fn linked_journals(
    discovery: &mut DiscoveryReads,
    directory: &Path,
    parent_path: &Path,
    parent_id: &str,
    journals: &mut Vec<Journal>,
) {
    let start = journals.len();
    let mut forks: HashMap<String, Vec<Journal>> = HashMap::new();
    discovery.walk(directory, |_, path, kind| {
        if kind != Kind::File || !is_journal_name(&path) {
            return Ok(());
        }
        let Some(journal) = read_journal(&path)? else {
            return Ok(());
        };
        let Some(parent) = journal
            .header
            .parent_session
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(());
        };
        if parent == parent_id || parent_is_path(parent, parent_path) {
            journals.push(journal);
        } else if !parent.contains(['/', '\\', ':']) {
            // Native forks name their source session ID instead of its file path.
            forks.entry(parent.clone()).or_default().push(journal);
        }
        Ok(())
    });
    // Only IDs reached from the named parent can admit sibling forks. Unrelated
    // journals and cycles in an artifact directory cannot seed discovery.
    let mut index = start;
    while index < journals.len() {
        if let Some(linked) = forks.remove(&journals[index].header.id) {
            journals.extend(linked);
        }
        index += 1;
    }
}

fn parent_is_path(declared: &str, parent: &Path) -> bool {
    // Lineage metadata never authorizes a filesystem lookup.
    local_absolute_path(Path::new(declared)).is_some_and(|declared| {
        #[cfg(windows)]
        return declared
            .as_os_str()
            .eq_ignore_ascii_case(parent.as_os_str());
        #[cfg(not(windows))]
        return declared == parent;
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Directory,
    File,
}

pub(super) enum LocalState<T> {
    Missing,
    Rejected,
    Present(T),
}

impl<T> LocalState<T> {
    fn present(self) -> Option<T> {
        match self {
            Self::Present(value) => Some(value),
            Self::Missing | Self::Rejected => None,
        }
    }
}

pub(super) type LocalFileState = LocalState<File>;

struct Opened {
    file: File,
    path: PathBuf,
}

pub(super) fn local_directory(path: &Path) -> io::Result<Option<PathBuf>> {
    let Some(path) = local_absolute_path(path) else {
        return Ok(None);
    };
    Ok(platform::open(&path, Kind::Directory)?
        .present()
        .map(|opened| opened.path))
}

/// Used before SQLite's path-based open. Unlike the journal opener, this is
/// only a preflight for a consumer that cannot read from our verified handle.
pub(super) fn local_file_state(path: &Path) -> io::Result<LocalFileState> {
    let Some(path) = local_absolute_path(path) else {
        return Ok(LocalState::Rejected);
    };
    Ok(match platform::open(&path, Kind::File)? {
        LocalState::Missing => LocalState::Missing,
        LocalState::Rejected => LocalState::Rejected,
        LocalState::Present(opened) => LocalState::Present(opened.file),
    })
}

/// Read errors inside an approved home never authorize a path or prove deletion.
/// Keep healthy siblings, but never prune unless every discovery read completed.
#[derive(Debug, Default)]
struct DiscoveryReads {
    incomplete: bool,
}

impl DiscoveryReads {
    fn read<T>(&mut self, result: io::Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(_) => {
                self.incomplete = true;
                None
            }
        }
    }

    fn directory(&mut self, path: &Path) -> Option<PathBuf> {
        self.read(local_directory(path)).flatten()
    }

    fn walk(
        &mut self,
        path: &Path,
        mut visit: impl FnMut(&mut Self, PathBuf, Kind) -> io::Result<()>,
    ) {
        let Some(Opened { file, path }) = self
            .read(platform::open(path, Kind::Directory))
            .and_then(LocalState::present)
        else {
            // This was a known directory. It may have changed since discovery.
            self.incomplete = true;
            return;
        };
        let result = platform::read_directory(&file, |name, kind| {
            if name != "." && name != ".." && valid_component(name) {
                let entry = visit(self, path.join(name), kind);
                self.read(entry);
            }
            Ok(())
        });
        self.read(result);
    }
}

fn local_absolute_path(path: &Path) -> Option<PathBuf> {
    let mut parts = path.components();
    #[cfg(windows)]
    let mut normalized = {
        use std::path::Prefix;
        let Component::Prefix(prefix) = parts.next()? else {
            return None;
        };
        let (Prefix::Disk(drive) | Prefix::VerbatimDisk(drive)) = prefix.kind() else {
            return None;
        };
        // A disk-verbatim prefix avoids Win32 path aliasing and matches canonicalized cache keys.
        PathBuf::from(format!(r"\\?\{}:\", char::from(drive.to_ascii_uppercase())))
    };
    #[cfg(not(windows))]
    let mut normalized = {
        if path.as_os_str().as_encoded_bytes().starts_with(b"//") {
            return None;
        }
        PathBuf::from(std::path::MAIN_SEPARATOR_STR)
    };
    if parts.next()? != Component::RootDir {
        return None;
    }
    for part in parts {
        match part {
            Component::Normal(name) if valid_component(name) => normalized.push(name),
            _ => return None,
        }
    }
    Some(normalized)
}

#[cfg(not(windows))]
fn valid_component(name: &OsStr) -> bool {
    !name.as_encoded_bytes().contains(&0)
}

#[cfg(windows)]
fn valid_component(name: &OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    if bytes
        .last()
        .is_some_and(|&byte| matches!(byte, b'.' | b' '))
        || bytes
            .iter()
            .any(|&byte| byte < 32 || b"<>:\"/\\|?*".contains(&byte))
    {
        return false;
    }
    let device = bytes.split(|&byte| byte == b'.').next().unwrap_or_default();
    let device = device.trim_ascii_end();
    [
        b"CON".as_slice(),
        b"PRN",
        b"AUX",
        b"NUL",
        b"CONIN$",
        b"CONOUT$",
        b"CLOCK$",
    ]
    .iter()
    .all(|name| !device.eq_ignore_ascii_case(name))
        && !device.get(..3).is_some_and(|prefix| {
            (prefix.eq_ignore_ascii_case(b"COM") || prefix.eq_ignore_ascii_case(b"LPT"))
                && matches!(&device[3..], [b'0'..=b'9'] | [0xc2, 0xb9 | 0xb2 | 0xb3])
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn journal(path: &Path, id: &str, parent: Option<&str>, with_title: bool) {
        let mut text = if with_title {
            "{\"type\":\"title\",\"title\":\"A session\"}\n".to_owned()
        } else {
            String::new()
        };
        let mut header = serde_json::json!({
            "type": "session",
            "id": id,
            "timestamp": "2026-01-01T00:00:00Z",
            "cwd": path.parent().unwrap(),
        });
        if let Some(parent) = parent {
            header["parentSession"] = serde_json::json!(parent);
        }
        text.push_str(&header.to_string());
        text.push('\n');
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn register(registry: &Path, marker: &str, target: &Path) {
        fs::create_dir_all(registry).unwrap();
        fs::write(registry.join(marker), target.to_str().unwrap()).unwrap();
    }

    #[test]
    fn unions_managed_and_exact_registered_logs_without_scanning_external_siblings() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let home = root.join("profile");
        let registry = home.join("agent/custom-session-files");
        let default = home.join("agent/sessions/project/legacy.jsonl");
        let managed = home.join("managed");
        let custom = managed.join("project/custom.jsonl");
        let child = managed.join("project/custom/child.jsonl");
        let unrelated = managed.join("project/custom/unrelated.jsonl");
        let spill = managed.join("project/custom/local/spill.jsonl");
        let external = root.join("external/session with spaces.jsonl");
        let neighbor = root.join("external/not-registered.jsonl");
        let malformed = root.join("external/artifact.jsonl");
        let extensionless = root.join("external/no-extension");

        fs::create_dir_all(default.parent().unwrap()).unwrap();
        fs::write(&default, "legacy file without a strict discovery signature").unwrap();
        journal(&custom, "custom", None, false);
        journal(&child, "child", Some("custom"), false);
        journal(&unrelated, "unrelated", None, false);
        journal(&spill, "spill", Some("custom"), false);
        journal(&external, "external", Some("unavailable-fork-source"), true);
        journal(&neighbor, "neighbor", Some("external"), false);
        journal(&extensionless, "no-extension", None, false);
        fs::write(&malformed, "{\"type\":\"session\"}\n").unwrap();
        register(&registry, "first", &external);
        register(&registry, "second", &external);
        register(&registry, "default-overlap", &default);
        register(&registry, "missing", &root.join("missing/session.jsonl"));
        register(&registry, "directory", external.parent().unwrap());
        register(&registry, "malformed", &malformed);
        register(&registry, "extensionless", &extensionless);
        fs::write(
            registry.join("large"),
            vec![b'x'; MAX_MARKER_BYTES as usize + 1],
        )
        .unwrap();
        fs::write(registry.join("relative"), "external/not-registered.jsonl").unwrap();
        fs::write(
            registry.join("multiline"),
            format!("{}\n{}\n", external.display(), neighbor.display()),
        )
        .unwrap();

        let files = discover_omp_files(&home, &[&managed, &home.join("not-created")]).unwrap();
        let mut legacy = String::new();
        files
            .open(&default)
            .unwrap()
            .unwrap()
            .read_to_string(&mut legacy)
            .unwrap();
        assert_eq!(legacy, "legacy file without a strict discovery signature");
        let mut expected = vec![default, custom, child, external];
        expected.sort_unstable();
        assert_eq!(files.paths(), expected);
    }

    #[test]
    fn follows_deep_parent_paths_and_fork_ids_but_not_unrooted_journals() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let home = root.join("profile");
        let registry = home.join("agent/custom-session-files");
        let parent = root.join("external/parent.jsonl");
        let child = root.join("external/parent/task.jsonl");
        let fork = root.join("external/parent/fork.jsonl");
        let grandchild = root.join("external/parent/fork/task.jsonl");
        let great_grandchild = root.join("external/parent/fork/task/worker.jsonl");
        journal(&parent, "parent-id", None, true);
        journal(&child, "task-id", parent.to_str(), true);
        journal(&fork, "fork-id", Some("task-id"), true);
        journal(&grandchild, "next-task", fork.to_str(), false);
        journal(&great_grandchild, "last-task", Some("next-task"), false);
        journal(
            &root.join("external/parent/cycle-a.jsonl"),
            "cycle-a",
            Some("cycle-b"),
            false,
        );
        journal(
            &root.join("external/parent/cycle-b.jsonl"),
            "cycle-b",
            Some("cycle-a"),
            false,
        );
        journal(
            &root.join("external/parent/somewhere/stray.jsonl"),
            "stray",
            Some("parent-id"),
            false,
        );
        register(&registry, "root", &parent);

        let mut expected = vec![parent, child, fork, grandchild, great_grandchild];
        expected.sort_unstable();
        assert_eq!(discover_omp_files(&home, &[]).unwrap().paths(), expected);
    }

    #[test]
    fn an_outside_managed_root_is_rejected_and_a_missing_root_is_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let home = root.join("profile");
        fs::create_dir(&home).unwrap();
        assert!(discover_omp_files(&home, &[&home.join("absent")])
            .unwrap()
            .paths()
            .is_empty());
        assert_eq!(
            discover_omp_files(&home, &[&root.join("outside")])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let file = home.join("ordinary-file");
        fs::write(&file, "").unwrap();
        assert!(local_directory(&file).unwrap().is_none());
        assert!(matches!(
            local_file_state(&home).unwrap(),
            LocalFileState::Rejected
        ));
        assert!(matches!(
            local_file_state(&home.join("missing")).unwrap(),
            LocalFileState::Missing
        ));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn links_are_rejected_at_the_registry_target_and_every_ancestor() {
        #[cfg(unix)]
        use std::os::unix::fs::{symlink as symlink_dir, symlink as symlink_file};
        #[cfg(windows)]
        use std::os::windows::fs::{symlink_dir, symlink_file};

        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let home = root.join("profile");
        let registry = home.join("agent/custom-session-files");
        let real = root.join("real/foreign.jsonl");
        let parent = root.join("allowed/parent.jsonl");
        journal(&real, "foreign", Some("parent"), false);
        journal(&parent, "parent", None, false);
        register(&registry, "valid", &parent);

        let linked = root.join("linked.jsonl");
        match symlink_file(&real, &linked) {
            // Windows without Developer Mode may disallow making test symlinks.
            #[cfg(windows)]
            Err(error) if error.raw_os_error() == Some(1314) => return,
            result => result.unwrap(),
        }
        register(&registry, "linked-file", &linked);
        let linked_dir = root.join("linked-directory");
        symlink_dir(real.parent().unwrap(), &linked_dir).unwrap();
        register(
            &registry,
            "linked-ancestor",
            &linked_dir.join("foreign.jsonl"),
        );
        let linked_marker = root.join("marker");
        fs::write(&linked_marker, real.to_str().unwrap()).unwrap();
        symlink_file(&linked_marker, registry.join("linked-marker")).unwrap();
        symlink_dir(real.parent().unwrap(), parent.with_extension("")).unwrap();
        let managed_link = home.join("linked-managed");
        symlink_dir(real.parent().unwrap(), &managed_link).unwrap();
        let default = home.join("agent/sessions");
        fs::create_dir(&default).unwrap();
        symlink_dir(real.parent().unwrap(), default.join("linked-default-entry")).unwrap();

        let files = discover_omp_files(&home, &[&managed_link]).unwrap();
        assert_eq!(files.paths(), [parent]);
        let linked_legacy = default.join("linked-default-entry/foreign.jsonl");
        assert!(files.accepts_legacy_path(&linked_legacy));
        assert!(files.open(&linked_legacy).is_err());
        assert!(matches!(
            local_file_state(&linked).unwrap(),
            LocalFileState::Rejected
        ));
        let linked_home = root.join("linked-profile");
        symlink_dir(home, &linked_home).unwrap();
        assert!(discover_omp_files(&linked_home, &[])
            .unwrap()
            .paths()
            .is_empty());
    }

    #[test]
    fn reopening_checks_lineage_and_distinguishes_missing_legacy_journals() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let home = root.join("profile");
        let managed = home.join("managed");
        let session = managed.join("root.jsonl");
        let legacy = home.join("agent/sessions/project/legacy.jsonl");
        journal(&session, "observed", Some("original-parent"), true);
        let files = discover_omp_files(&home, &[&managed]).unwrap();
        let mut text = String::new();
        files
            .open(&session)
            .unwrap()
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert!(text.starts_with("{\"type\":\"title\""));

        journal(&session, "observed", Some("different-parent"), true);
        assert!(files.open(&session).is_err());
        journal(&session, "different-id", Some("original-parent"), true);
        assert!(files.open(&session).is_err());

        assert!(files.accepts_legacy_path(&legacy));
        assert!(files.open(&legacy).unwrap().is_none());
        assert!(files.open(&root.join("unlisted.jsonl")).is_err());
        journal(&legacy, "not-discovered", None, false);
        assert!(files.open(&legacy).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn rejects_windows_network_devices_ads_and_noncanonical_components() {
        for value in [
            r"\\server\share\session.jsonl",
            r"\\?\UNC\server\share\session.jsonl",
            r"\\.\pipe\session.jsonl",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\session.jsonl",
            r"C:session.jsonl",
            r"C:\work\file.jsonl:stream.jsonl",
            r"C:\work\NUL.jsonl",
            r"C:\work\COM¹.jsonl",
            r"C:\work.\session.jsonl",
            r"C:\work\..\session.jsonl",
        ] {
            assert!(local_absolute_path(Path::new(value)).is_none(), "{value}");
        }
        assert_eq!(
            local_absolute_path(Path::new("c:/folder/session.jsonl")),
            Some(PathBuf::from(r"\\?\C:\folder\session.jsonl"))
        );
    }
}
