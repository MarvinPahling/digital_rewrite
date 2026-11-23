//! Bounded filesystem adapter and atomic publisher for the supported `.dig` document subset.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
pub const MAX_OPEN_BYTES: usize = 4 * 1024 * 1024;
const TEMP_ATTEMPTS: usize = 16;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardIntent {
    RefuseDirty,
    DiscardDirty,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveAsIntent {
    CreateOnly,
    Replace,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStage {
    ResolvePath,
    ValidateTarget,
    Read,
    CreateTemp,
    Write,
    Flush,
    SyncFile,
    Publish,
    RemoveTemp,
    SyncParent,
}
impl std::fmt::Display for FileStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ResolvePath => "resolve path",
            Self::ValidateTarget => "validate target",
            Self::Read => "read",
            Self::CreateTemp => "create staging file",
            Self::Write => "write staging file",
            Self::Flush => "flush staging file",
            Self::SyncFile => "sync staging file",
            Self::Publish => "publish",
            Self::RemoveTemp => "remove staging file",
            Self::SyncParent => "sync parent directory",
        })
    }
}
#[derive(Debug)]
pub struct FileOperationError {
    pub stage: FileStage,
    pub path: PathBuf,
    pub source: io::Error,
}
impl std::fmt::Display for FileOperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} '{}' failed: {}",
            self.stage,
            self.path.display(),
            self.source
        )
    }
}
impl std::error::Error for FileOperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
#[derive(Debug)]
pub struct FileDiagnostic {
    pub stage: FileStage,
    pub path: PathBuf,
    pub source: io::Error,
}
impl std::fmt::Display for FileDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} '{}' failed: {}",
            self.stage,
            self.path.display(),
            self.source
        )
    }
}
impl std::error::Error for FileDiagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
#[derive(Debug)]
pub enum PublishOutcome {
    Published,
    PublishedDurabilityUnconfirmed(Vec<FileDiagnostic>),
}
impl PartialEq for PublishOutcome {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Published, Self::Published) => true,
            (Self::PublishedDurabilityUnconfirmed(a), Self::PublishedDurabilityUnconfirmed(b)) => {
                a.len() == b.len()
                    && a.iter().zip(b).all(|(a, b)| {
                        a.stage == b.stage
                            && a.path == b.path
                            && a.source.kind() == b.source.kind()
                            && a.source.to_string() == b.source.to_string()
                    })
            }
            _ => false,
        }
    }
}

pub trait StagedFile {
    fn path(&self) -> &Path;
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
    fn sync_close(self: Box<Self>) -> io::Result<()>;
}
pub trait FileIo {
    fn base_dir(&self) -> &Path;
    fn read_bounded_regular(&mut self, path: &Path, limit: usize) -> io::Result<Vec<u8>>;
    fn validate_target(&mut self, target: &Path, intent: SaveAsIntent) -> io::Result<()>;
    fn create_temp(&mut self, path: &Path) -> io::Result<Box<dyn StagedFile>>;
    fn publish(&mut self, temp: &Path, target: &Path, intent: SaveAsIntent) -> io::Result<()>;
    fn sync_parent(&mut self, parent: &Path) -> io::Result<()>;
    fn remove_temp(&mut self, temp: &Path) -> io::Result<()>;
}
#[derive(Default)]
pub struct LocalFileIo {
    base_dir: PathBuf,
}
impl LocalFileIo {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }
}
struct LocalStage {
    path: PathBuf,
    file: Option<File>,
}
impl StagedFile for LocalStage {
    fn path(&self) -> &Path {
        &self.path
    }
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("staging file is closed"))?
            .write_all(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("staging file is closed"))?
            .flush()
    }
    fn sync_close(mut self: Box<Self>) -> io::Result<()> {
        let Some(file) = self.file.take() else {
            return Err(io::Error::other("staging file is closed"));
        };
        file.sync_all()?;
        drop(file);
        Ok(())
    }
}
impl FileIo for LocalFileIo {
    fn base_dir(&self) -> &Path {
        &self.base_dir
    }
    fn read_bounded_regular(&mut self, path: &Path, limit: usize) -> io::Result<Vec<u8>> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only regular files can be opened; symlinks and non-regular files are refused",
            ));
        }
        let file = File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "opened path is not a regular file",
            ));
        }
        let cap = limit
            .checked_add(1)
            .ok_or_else(|| io::Error::other("read limit overflow"))?;
        let mut bytes = Vec::with_capacity(cap.min(64 * 1024));
        file.take(cap as u64).read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file exceeds the 4 MiB open limit",
            ));
        }
        Ok(bytes)
    }
    fn validate_target(&mut self, target: &Path, intent: SaveAsIntent) -> io::Result<()> {
        match fs::symlink_metadata(target) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "save target must be a regular file; symlinks and directories are refused",
            )),
            Ok(_) if intent == SaveAsIntent::CreateOnly => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "create-only target already exists",
            )),
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
    fn create_temp(&mut self, path: &Path) -> io::Result<Box<dyn StagedFile>> {
        let file = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(path)?;
        Ok(Box::new(LocalStage {
            path: path.to_path_buf(),
            file: Some(file),
        }))
    }
    fn publish(&mut self, temp: &Path, target: &Path, intent: SaveAsIntent) -> io::Result<()> {
        match intent {
            SaveAsIntent::Replace => fs::rename(temp, target),
            SaveAsIntent::CreateOnly => fs::hard_link(temp, target),
        }
    }
    fn sync_parent(&mut self, parent: &Path) -> io::Result<()> {
        File::open(parent)?.sync_all()
    }
    fn remove_temp(&mut self, temp: &Path) -> io::Result<()> {
        fs::remove_file(temp)
    }
}

/// Resolve relative paths against the caller's explicit absolute base; never consult cwd or require UTF-8.
pub fn resolve_path(base: &Path, path: &Path) -> io::Result<PathBuf> {
    if !base.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "filesystem base directory must be absolute",
        ));
    }
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Prefix(p) => result.push(p.as_os_str()),
            Component::RootDir => result.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            Component::Normal(p) => result.push(p),
        }
    }
    if result.is_absolute() {
        Ok(result)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resolved path is not absolute",
        ))
    }
}

pub fn publish_bytes(
    io: &mut dyn FileIo,
    target: &Path,
    bytes: &[u8],
    intent: SaveAsIntent,
) -> Result<PublishOutcome, PublishFailure> {
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            PublishFailure::not_published(
                FileStage::ValidateTarget,
                target.to_path_buf(),
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "target has no parent directory",
                ),
                None,
            )
        })?;
    io.validate_target(target, intent).map_err(|e| {
        PublishFailure::not_published(FileStage::ValidateTarget, target.to_path_buf(), e, None)
    })?;
    let mut staged = None;
    let mut collision = None;
    for _ in 0..TEMP_ATTEMPTS {
        let n = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".digital-rust-{}-{n}.tmp", std::process::id()));
        match io.create_temp(&candidate) {
            Ok(file) => {
                staged = Some(file);
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => collision = Some((candidate, e)),
            Err(e) => {
                return Err(PublishFailure::not_published(
                    FileStage::CreateTemp,
                    candidate,
                    e,
                    None,
                ));
            }
        }
    }
    let Some(mut file) = staged else {
        let (path, error) = collision.unwrap_or_else(|| {
            (
                target.to_path_buf(),
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "could not reserve unique staging file",
                ),
            )
        });
        return Err(PublishFailure::not_published(
            FileStage::CreateTemp,
            path,
            error,
            None,
        ));
    };
    let temp = file.path().to_path_buf();
    if let Err(primary) = file.write_all(bytes) {
        drop(file);
        let cleanup = io
            .remove_temp(&temp)
            .err()
            .map(|source| (FileStage::RemoveTemp, temp.clone(), source));
        return Err(PublishFailure::not_published(
            FileStage::Write,
            temp,
            primary,
            cleanup,
        ));
    }
    if let Err(primary) = file.flush() {
        drop(file);
        let cleanup = io
            .remove_temp(&temp)
            .err()
            .map(|source| (FileStage::RemoveTemp, temp.clone(), source));
        return Err(PublishFailure::not_published(
            FileStage::Flush,
            temp,
            primary,
            cleanup,
        ));
    }
    if let Err(primary) = file.sync_close() {
        let cleanup = io
            .remove_temp(&temp)
            .err()
            .map(|source| (FileStage::RemoveTemp, temp.clone(), source));
        return Err(PublishFailure::not_published(
            FileStage::SyncFile,
            temp,
            primary,
            cleanup,
        ));
    }
    if let Err(primary) = io.publish(&temp, target, intent) {
        let cleanup = io
            .remove_temp(&temp)
            .err()
            .map(|source| (FileStage::RemoveTemp, temp.clone(), source));
        return Err(PublishFailure::not_published(
            FileStage::Publish,
            target.to_path_buf(),
            primary,
            cleanup,
        ));
    }
    let mut warnings = Vec::new();
    if intent == SaveAsIntent::CreateOnly
        && let Err(source) = io.remove_temp(&temp)
    {
        warnings.push(FileDiagnostic {
            stage: FileStage::RemoveTemp,
            path: temp.clone(),
            source,
        });
    }
    if let Err(source) = io.sync_parent(parent) {
        warnings.push(FileDiagnostic {
            stage: FileStage::SyncParent,
            path: parent.to_path_buf(),
            source,
        });
    }
    Ok(if warnings.is_empty() {
        PublishOutcome::Published
    } else {
        PublishOutcome::PublishedDurabilityUnconfirmed(warnings)
    })
}
#[derive(Debug)]
pub struct PublishFailure {
    pub stage: FileStage,
    pub path: PathBuf,
    pub primary: io::Error,
    pub cleanup: Option<FileDiagnostic>,
}
impl PublishFailure {
    fn not_published(
        stage: FileStage,
        path: PathBuf,
        primary: io::Error,
        cleanup: Option<(FileStage, PathBuf, io::Error)>,
    ) -> Self {
        Self {
            stage,
            path,
            primary,
            cleanup: cleanup.map(|(stage, path, source)| FileDiagnostic {
                stage,
                path,
                source,
            }),
        }
    }
}
impl std::fmt::Display for PublishFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "save failed during {} at '{}': {}",
            self.stage,
            self.path.display(),
            self.primary
        )?;
        if let Some(warning) = &self.cleanup {
            write!(f, "; cleanup also failed: {warning}")?;
        }
        Ok(())
    }
}
impl std::error::Error for PublishFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.primary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "digital-rust-{label}-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Fail {
        None,
        Create,
        PartialWrite,
        Flush,
        Sync,
        Publish,
        SyncParent,
        Cleanup,
        PostPublishCleanup,
        BothPostPublish,
        ReadPermission,
    }
    struct FaultFs {
        real: LocalFileIo,
        fail: Fail,
        collide_once: bool,
    }
    struct FaultStage {
        inner: Box<dyn StagedFile>,
        fail: Fail,
    }
    impl StagedFile for FaultStage {
        fn path(&self) -> &Path {
            self.inner.path()
        }
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            if self.fail == Fail::PartialWrite {
                self.inner.write_all(&bytes[..bytes.len().min(3)])?;
                return Err(io::Error::other("injected partial write"));
            }
            self.inner.write_all(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.fail == Fail::Flush {
                return Err(io::Error::other("injected flush failure"));
            }
            self.inner.flush()
        }
        fn sync_close(self: Box<Self>) -> io::Result<()> {
            if self.fail == Fail::Sync {
                return Err(io::Error::other("injected file sync failure"));
            }
            self.inner.sync_close()
        }
    }
    impl FileIo for FaultFs {
        fn base_dir(&self) -> &Path {
            self.real.base_dir()
        }
        fn read_bounded_regular(&mut self, p: &Path, n: usize) -> io::Result<Vec<u8>> {
            if self.fail == Fail::ReadPermission {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected read permission failure",
                ));
            }
            self.real.read_bounded_regular(p, n)
        }
        fn validate_target(&mut self, p: &Path, i: SaveAsIntent) -> io::Result<()> {
            self.real.validate_target(p, i)
        }
        fn create_temp(&mut self, p: &Path) -> io::Result<Box<dyn StagedFile>> {
            if self.fail == Fail::Create {
                return Err(io::Error::other("injected create failure"));
            }
            if self.collide_once {
                self.collide_once = false;
                fs::write(p, b"foreign")?;
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            self.real.create_temp(p).map(|inner| {
                Box::new(FaultStage {
                    inner,
                    fail: if self.fail == Fail::Cleanup {
                        Fail::PartialWrite
                    } else {
                        self.fail
                    },
                }) as Box<dyn StagedFile>
            })
        }
        fn publish(&mut self, t: &Path, p: &Path, i: SaveAsIntent) -> io::Result<()> {
            if self.fail == Fail::Publish {
                return Err(io::Error::other("injected publish failure"));
            }
            if !p.exists() && p.file_name().is_some_and(|n| n == "raced.dig") {
                fs::write(p, b"concurrent writer")?;
            }
            self.real.publish(t, p, i)
        }
        fn sync_parent(&mut self, p: &Path) -> io::Result<()> {
            if matches!(self.fail, Fail::SyncParent | Fail::BothPostPublish) {
                return Err(io::Error::other("injected directory sync failure"));
            }
            self.real.sync_parent(p)
        }
        fn remove_temp(&mut self, p: &Path) -> io::Result<()> {
            if matches!(
                self.fail,
                Fail::Cleanup | Fail::PostPublishCleanup | Fail::BothPostPublish
            ) {
                return Err(io::Error::other("injected cleanup failure"));
            }
            self.real.remove_temp(p)
        }
    }
    fn adapter(dir: &Path, fail: Fail) -> FaultFs {
        FaultFs {
            real: LocalFileIo::new(dir),
            fail,
            collide_once: false,
        }
    }
    #[test]
    fn prepublication_faults_preserve_target_and_cleanup_errors_are_reported() {
        let dir = temp_dir("atomic-failures");
        let target = dir.join("doc.dig");
        for fail in [
            Fail::Create,
            Fail::PartialWrite,
            Fail::Flush,
            Fail::Sync,
            Fail::Publish,
        ] {
            fs::write(&target, b"old bytes").unwrap();
            assert!(
                publish_bytes(
                    &mut adapter(&dir, fail),
                    &target,
                    b"new",
                    SaveAsIntent::Replace,
                )
                .is_err()
            );
            assert_eq!(fs::read(&target).unwrap(), b"old bytes");
        }
        let mut failing = adapter(&dir, Fail::PartialWrite);
        failing.fail = Fail::Cleanup;
        assert!(
            publish_bytes(&mut failing, &target, b"x", SaveAsIntent::Replace)
                .unwrap_err()
                .cleanup
                .is_some()
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn open_failures_retain_stage_path_and_underlying_error_kind() {
        let dir = temp_dir("open-error-context");
        let mut session = crate::engine::Session::default();
        let missing = dir.join("missing.dig");
        let error = session
            .open_path(
                &mut adapter(&dir, Fail::None),
                Path::new("missing.dig"),
                crate::file_session::DiscardIntent::DiscardDirty,
            )
            .unwrap_err();
        let crate::engine::SessionDigError::File(error) = error else {
            panic!("expected file error")
        };
        assert_eq!(error.stage, FileStage::Read);
        assert_eq!(error.path, missing);
        assert_eq!(error.source.kind(), io::ErrorKind::NotFound);
        let denied_path = dir.join("private.dig");
        let error = session
            .open_path(
                &mut adapter(&dir, Fail::ReadPermission),
                Path::new("private.dig"),
                crate::file_session::DiscardIntent::DiscardDirty,
            )
            .unwrap_err();
        let crate::engine::SessionDigError::File(error) = error else {
            panic!("expected file error")
        };
        assert_eq!(error.stage, FileStage::Read);
        assert_eq!(error.path, denied_path);
        assert_eq!(error.source.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("private.dig"));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn post_publish_cleanup_failure_is_warning_and_commits_session() {
        let dir = temp_dir("published-cleanup");
        let mut session = crate::engine::Session::default();
        session
            .place(
                crate::domain::ComponentKind::Input,
                crate::domain::Position { x: 0.0, y: 0.0 },
            )
            .unwrap();
        let target = dir.join("created.dig");
        let outcome = session
            .save_as(
                &mut adapter(&dir, Fail::PostPublishCleanup),
                Path::new("created.dig"),
                SaveAsIntent::CreateOnly,
            )
            .unwrap();
        let PublishOutcome::PublishedDurabilityUnconfirmed(warnings) = outcome else {
            panic!("expected published-with-cleanup-warning outcome");
        };
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].stage, FileStage::RemoveTemp);
        assert!(warnings[0].path.exists());
        assert!(target.exists());
        assert!(!session.dirty());
        assert_eq!(session.path(), Some(target.as_path()));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn post_publish_cleanup_and_directory_sync_keep_both_structured_warnings_and_commit_session() {
        let dir = temp_dir("published-cleanup-and-sync");
        let mut session = crate::engine::Session::default();
        session
            .place(
                crate::domain::ComponentKind::Input,
                crate::domain::Position { x: 0.0, y: 0.0 },
            )
            .unwrap();
        let target = dir.join("created.dig");
        let outcome = session
            .save_as(
                &mut adapter(&dir, Fail::BothPostPublish),
                Path::new("created.dig"),
                SaveAsIntent::CreateOnly,
            )
            .unwrap();
        let PublishOutcome::PublishedDurabilityUnconfirmed(warnings) = outcome else {
            panic!("expected published-with-warnings outcome");
        };
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].stage, FileStage::RemoveTemp);
        assert_eq!(warnings[0].path.parent(), Some(dir.as_path()));
        assert!(
            warnings[0].path.exists(),
            "leftover temp path must be reported and retained"
        );
        assert_eq!(warnings[1].stage, FileStage::SyncParent);
        assert_eq!(warnings[1].path, dir);
        assert!(
            target.exists(),
            "publication must succeed despite post-publish failures"
        );
        assert!(!session.dirty());
        assert_eq!(session.path(), Some(target.as_path()));
        assert!(!fs::read(target).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn retries_foreign_temp_collision_and_create_only_race_without_overwrite() {
        let dir = temp_dir("collision");
        let target = dir.join("doc.dig");
        let mut io = adapter(&dir, Fail::None);
        io.collide_once = true;
        publish_bytes(&mut io, &target, b"ours", SaveAsIntent::CreateOnly).unwrap();
        let foreign: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "tmp"))
            .collect();
        assert_eq!(foreign.len(), 1);
        assert_eq!(fs::read(&foreign[0]).unwrap(), b"foreign");
        let raced = dir.join("raced.dig");
        assert!(
            publish_bytes(
                &mut adapter(&dir, Fail::None),
                &raced,
                b"new",
                SaveAsIntent::CreateOnly
            )
            .is_err()
        );
        assert_eq!(fs::read(raced).unwrap(), b"concurrent writer");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn directory_sync_failure_is_a_published_warning_and_read_is_bounded() {
        let dir = temp_dir("sync-warning");
        let target = dir.join("doc.dig");
        let PublishOutcome::PublishedDurabilityUnconfirmed(warnings) = publish_bytes(
            &mut adapter(&dir, Fail::SyncParent),
            &target,
            b"x",
            SaveAsIntent::CreateOnly,
        )
        .unwrap() else {
            panic!("expected durability warning")
        };
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].stage, FileStage::SyncParent);
        assert_eq!(warnings[0].path, dir);
        assert_eq!(fs::read(&target).unwrap(), b"x");
        let mut session = crate::engine::Session::default();
        session
            .place(
                crate::domain::ComponentKind::Input,
                crate::domain::Position { x: 0.0, y: 0.0 },
            )
            .unwrap();
        let published = session
            .save_as(
                &mut adapter(&dir, Fail::SyncParent),
                Path::new("session.dig"),
                SaveAsIntent::CreateOnly,
            )
            .unwrap();
        assert!(matches!(
            published,
            PublishOutcome::PublishedDurabilityUnconfirmed(_)
        ));
        assert!(!session.dirty());
        assert_eq!(session.path(), Some(dir.join("session.dig").as_path()));
        let directory_target = dir.join("directory-target");
        fs::create_dir(&directory_target).unwrap();
        assert!(
            publish_bytes(
                &mut LocalFileIo::new(&dir),
                &directory_target,
                b"x",
                SaveAsIntent::Replace
            )
            .is_err()
        );
        let large = dir.join("large");
        fs::write(&large, vec![b'x'; MAX_OPEN_BYTES + 1]).unwrap();
        assert_eq!(
            adapter(&dir, Fail::None)
                .read_bounded_regular(&large, MAX_OPEN_BYTES)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            adapter(&dir, Fail::None)
                .read_bounded_regular(&dir, MAX_OPEN_BYTES)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        #[cfg(unix)]
        {
            let link = dir.join("link");
            std::os::unix::fs::symlink(&large, &link).unwrap();
            assert_eq!(
                adapter(&dir, Fail::None)
                    .read_bounded_regular(&link, MAX_OPEN_BYTES)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn real_replace_and_non_utf8_paths() {
        let dir = temp_dir("real");
        let target = dir.join("doc.dig");
        publish_bytes(
            &mut LocalFileIo::new(&dir),
            &target,
            b"old",
            SaveAsIntent::CreateOnly,
        )
        .unwrap();
        publish_bytes(
            &mut LocalFileIo::new(&dir),
            &target,
            b"new",
            SaveAsIntent::Replace,
        )
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let p = PathBuf::from(std::ffi::OsString::from_vec(b"x-\xff".to_vec()));
            assert_eq!(resolve_path(&dir, &p).unwrap().file_name(), p.file_name());
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
