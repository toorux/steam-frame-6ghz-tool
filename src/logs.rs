//! Per-run logs beside the original executable, never relative to the service CWD.
use crate::{backend, protocol::Result};
use std::{
    cell::{Cell, RefCell},
    fs::{self, File, OpenOptions},
    hash::{DefaultHasher, Hash, Hasher},
    io::{Read, Write},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Component, Path, PathBuf, Prefix},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime},
};
use windows_sys::Win32::Storage::FileSystem::*;

const MAX_SIZE: u64 = 1_048_576;
const KEEP_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn beside(exe: &Path) -> Result<PathBuf> {
    Ok(exe.parent().ok_or("无法确定程序目录")?.join("logs"))
}

fn local_absolute(path: &Path) -> bool {
    path.is_absolute()
        && matches!(path.components().next(), Some(Component::Prefix(p))
            if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        && !path.components().any(|c| {
            matches!(c, Component::ParentDir)
                || matches!(c, Component::Normal(s) if s.to_string_lossy().contains(':'))
        })
}

// Pin every directory while privileged logging is active. Reject junctions and
// symlinks; no FILE_SHARE_DELETE means the checked path cannot be renamed away.
fn lock_directories(dir: &Path) -> Result<Vec<File>> {
    if !local_absolute(dir) {
        return Err("日志目录必须是本地绝对路径，不能含链接或上级路径".into());
    }
    let mut handles = Vec::new();
    for ancestor in dir.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let handle = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(ancestor)
            .map_err(|e| format!("无法访问日志目录 {}：{e}", ancestor.display()))?;
        let metadata = handle.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(format!(
                "日志目录包含链接或不是目录：{}",
                ancestor.display()
            ));
        }
        handles.push(handle);
    }
    Ok(handles)
}

fn files(dir: &Path, prefix: &str) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name
            .strip_prefix(prefix)
            .and_then(|s| s.strip_prefix('-'))
            .and_then(|s| s.strip_suffix(".log"))
            .is_some_and(|s| {
                !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit() || b"-TZ".contains(&c))
            })
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

pub struct Session {
    pub dir: PathBuf,
    prefix: &'static str,
    file: RefCell<File>,
    last_cleanup: Cell<Instant>,
    _directories: Vec<File>,
}
impl Session {
    pub fn new(dir: &Path) -> Result<Self> {
        Self::new_named(dir, "auto")
    }
    pub fn new_program(dir: &Path) -> Result<Self> {
        Self::new_named(dir, "program")
    }
    fn new_named(dir: &Path, prefix: &'static str) -> Result<Self> {
        if !local_absolute(dir) {
            return Err("日志目录必须是本地绝对路径，不能含上级路径或数据流".into());
        }
        let parent = dir.parent().ok_or("无效的日志目录")?;
        let mut directories = lock_directories(parent)?;
        match fs::create_dir(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("创建日志目录失败：{e}")),
        }
        directories.extend(lock_directories(dir)?);
        let file = Self::new_file(dir, prefix)?;
        Self::cleanup(dir)?;
        Ok(Self {
            dir: dir.to_owned(),
            prefix,
            file: RefCell::new(file),
            last_cleanup: Cell::new(Instant::now()),
            _directories: directories,
        })
    }
    fn new_file(dir: &Path, prefix: &str) -> Result<File> {
        let file = loop {
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let stamp = backend::timestamp().replace([':', '.'], "");
            let name = format!("{prefix}-{stamp}-{}-{sequence:020}.log", std::process::id());
            // Never append to a pre-existing, potentially attacker-controlled file.
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .share_mode(FILE_SHARE_READ)
                .open(dir.join(name))
            {
                Ok(file) => break file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("创建执行日志失败：{e}")),
            }
        };
        Ok(file)
    }
    fn cleanup(dir: &Path) -> Result<()> {
        let now = SystemTime::now();
        for path in files(dir, "auto")?.into_iter().chain(files(dir, "program")?) {
            let Ok(metadata) = fs::symlink_metadata(&path) else { continue };
            if metadata.is_file()
                && metadata.modified().ok()
                    .and_then(|modified| now.duration_since(modified).ok())
                    .is_some_and(|age| age >= KEEP_AGE)
            {
                // An active file stays locked; retry it at the next cleanup.
                let _ = fs::remove_file(path);
            }
        }
        Ok(())
    }
    pub fn write(&self, text: &str) -> Result<()> {
        if self.last_cleanup.get().elapsed() >= CLEANUP_INTERVAL {
            Self::cleanup(&self.dir)?;
            self.last_cleanup.set(Instant::now());
        }
        let mut file = self.file.borrow_mut();
        if file.metadata().map_err(|e| e.to_string())?.len() >= MAX_SIZE {
            *file = Self::new_file(&self.dir, self.prefix)?;
        }
        writeln!(file, "[{}] {text}", backend::timestamp()).map_err(|e| e.to_string())?;
        file.sync_data().map_err(|e| e.to_string())
    }
}

pub fn read(dir: &Path) -> Result<String> {
    if !dir.try_exists().map_err(|e| e.to_string())? {
        return Ok(String::new());
    }
    let _directories = lock_directories(dir)?;
    let paths = files(dir, "auto")?;
    let mut text = String::new();
    for path in &paths {
        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|e| e.to_string())?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if info.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0
            || info.nNumberOfLinks != 1
        {
            return Err("拒绝读取链接或非普通日志文件".into());
        }
        text.push_str(&format!(
            "--- {} ---\n",
            path.file_name().unwrap().to_string_lossy()
        ));
        file.take(MAX_SIZE + 65_536)
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
    }
    Ok(text)
}

/// Cheap snapshot key: the UI only reloads text when retained files change.
pub fn revision(dir: &Path) -> Result<u64> {
    let mut hash = DefaultHasher::new();
    dir.hash(&mut hash);
    if dir.try_exists().map_err(|e| e.to_string())? {
        let _directories = lock_directories(dir)?;
        for path in files(dir, "auto")? {
            let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            path.hash(&mut hash);
            metadata.len().hash(&mut hash);
            metadata
                .modified()
                .map_err(|e| e.to_string())?
                .hash(&mut hash);
        }
    }
    Ok(hash.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logs_use_executable_parent_not_working_directory_or_service_copy() {
        assert_eq!(
            beside(Path::new(r"F:\tools\steam-frame-6ghz-tool.exe")).unwrap(),
            PathBuf::from(r"F:\tools\logs")
        );
        assert!(local_absolute(Path::new(r"F:\tools\logs")));
        for path in [
            r"logs",
            r"F:logs",
            r"F:\tools\..\logs",
            r"\\server\share\logs",
            r"F:\tools:stream\logs",
        ] {
            assert!(!local_absolute(Path::new(path)), "{path}");
        }
    }
    #[test]
    fn cleanup_removes_only_named_logs_older_than_30_days() {
        let dir = std::env::temp_dir().join(format!("steam-frame-log-test-{}-{}",
            std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        let old = dir.join("auto-20260101T000000Z-1-00000000000000000000.log");
        let old_program = dir.join("program-20260101T000000Z-1-00000000000000000000.log");
        let fresh = dir.join("auto-20260101T000000Z-1-00000000000000000001.log");
        let other = dir.join("other.log");
        let file = File::create(&old).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(
            SystemTime::now() - KEEP_AGE - Duration::from_secs(60))).unwrap();
        drop(file);
        let file = File::create(&old_program).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(
            SystemTime::now() - KEEP_AGE - Duration::from_secs(60))).unwrap();
        drop(file);
        File::create(&fresh).unwrap();
        File::create(&other).unwrap();
        Session::cleanup(&dir).unwrap();
        assert!(!old.exists());
        assert!(!old_program.exists());
        assert!(fresh.exists() && other.exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn program_logs_do_not_appear_in_service_log_reader() {
        let dir = std::env::temp_dir().join(format!("steam-frame-program-log-test-{}-{}",
            std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
        let program = Session::new_program(&dir).unwrap();
        program.write("program only").unwrap();
        assert!(read(&dir).unwrap().is_empty());
        let service = Session::new(&dir).unwrap();
        service.write("service only").unwrap();
        let text = read(&dir).unwrap();
        assert!(text.contains("service only"));
        assert!(!text.contains("program only"));
        drop((program, service));
        fs::remove_dir_all(dir).unwrap();
    }
}
