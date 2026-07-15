//! Deterministic content-addressed workspace checkpoints.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};

use epoch_blob::{BlobError, BlobHash, BlobStore};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const WORKSPACE_SCHEMA_VERSION: u32 = 1;
pub const MANIFEST_MEDIA_TYPE: &str = "application/vnd.epoch.workspace-manifest+json";
const FILE_MEDIA_TYPE: &str = "application/octet-stream";

/// Resource limits applied during both snapshot and restore validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceLimits {
    pub max_entries: usize,
    pub max_total_bytes: u64,
    pub max_file_bytes: u64,
    pub max_manifest_bytes: u64,
    pub max_depth: usize,
    pub max_path_bytes: usize,
    pub max_component_bytes: usize,
    pub max_symlink_target_bytes: usize,
}

impl Default for WorkspaceLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_total_bytes: 512 * 1024 * 1024,
            max_file_bytes: 256 * 1024 * 1024,
            max_manifest_bytes: 16 * 1024 * 1024,
            max_depth: 64,
            max_path_bytes: 4_096,
            max_component_bytes: 255,
            max_symlink_target_bytes: 4_096,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HardlinkPolicy {
    /// Files sharing an inode are restored as independent regular files with the same content.
    Materialize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceManifest {
    pub schema_version: u32,
    pub hardlink_policy: HardlinkPolicy,
    pub root_mode: u32,
    pub entries: Vec<WorkspaceEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceEntry {
    pub path: String,
    pub kind: EntryKind,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryKind {
    Directory {
        mode: u32,
    },
    Regular {
        mode: u32,
        executable: bool,
        length: u64,
        content_hash: BlobHash,
    },
    Symlink {
        target: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSnapshot {
    manifest_hash: BlobHash,
    manifest_length: u64,
}

impl WorkspaceSnapshot {
    #[must_use]
    pub const fn new(manifest_hash: BlobHash, manifest_length: u64) -> Self {
        Self {
            manifest_hash,
            manifest_length,
        }
    }

    #[must_use]
    pub const fn manifest_hash(&self) -> &BlobHash {
        &self.manifest_hash
    }

    #[must_use]
    pub const fn manifest_length(&self) -> u64 {
        self.manifest_length
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestoreReport {
    pub entries: usize,
    pub total_file_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestoreFault {
    None,
    AfterStagingCreated,
    AfterFirstEntry,
    BeforePublish,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitKind {
    Entries,
    TotalBytes,
    FileBytes,
    ManifestBytes,
    Depth,
    PathBytes,
    ComponentBytes,
    SymlinkTargetBytes,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Unsupported {
    NonUtf8Path,
    NonUtf8SymlinkTarget,
    SymlinkEscape { path: String },
    SpecialFile { path: String, kind: &'static str },
    SymlinksOnPlatform,
}

/// Full-copy control backend using immutable content-addressed file blobs.
pub struct WorkspaceBackend {
    blobs: Arc<BlobStore>,
    blob_root: PathBuf,
    limits: WorkspaceLimits,
}

impl WorkspaceBackend {
    /// Opens a backend rooted at trusted content-addressed storage.
    ///
    /// # Errors
    ///
    /// Returns an error for zero/contradictory limits or an unsafe blob-store path.
    pub fn open(
        blob_root: impl AsRef<Path>,
        limits: WorkspaceLimits,
    ) -> Result<Self, WorkspaceError> {
        validate_limits(limits)?;
        let maximum_blob = limits.max_file_bytes.max(limits.max_manifest_bytes);
        let blobs = BlobStore::open_with_max_blob_bytes(&blob_root, maximum_blob)?;
        let blob_root = fs::canonicalize(blob_root)?;
        Ok(Self {
            blobs: Arc::new(blobs),
            blob_root,
            limits,
        })
    }

    /// Captures a deterministic full-copy snapshot without following symlinks.
    ///
    /// # Errors
    ///
    /// Returns a typed unsupported, limit, race, filesystem, serialization, or blob error.
    pub fn snapshot(&self, source: impl AsRef<Path>) -> Result<WorkspaceSnapshot, WorkspaceError> {
        let source = source.as_ref();
        let root_metadata = fs::symlink_metadata(source)?;
        if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
            return Err(WorkspaceError::InvalidSourceRoot);
        }
        let canonical_source = fs::canonicalize(source)?;
        if canonical_source == self.blob_root || canonical_source.starts_with(&self.blob_root) {
            return Err(WorkspaceError::StateRootOverlapsWorkspace);
        }
        let mut capture = Capture {
            backend: self,
            source: &canonical_source,
            entries: Vec::new(),
            total_bytes: 0,
        };
        capture.walk(Path::new(""), 0)?;
        capture
            .entries
            .sort_unstable_by(|left, right| left.path.cmp(&right.path));
        let manifest = WorkspaceManifest {
            schema_version: WORKSPACE_SCHEMA_VERSION,
            hardlink_policy: HardlinkPolicy::Materialize,
            root_mode: metadata_mode(&root_metadata, true),
            entries: capture.entries,
        };
        validate_manifest(&manifest, self.limits)?;
        let encoded = serde_json::to_vec(&manifest)?;
        enforce_limit(
            LimitKind::ManifestBytes,
            u64::try_from(encoded.len()).unwrap_or(u64::MAX),
            self.limits.max_manifest_bytes,
        )?;
        let metadata = self.blobs.put(&encoded, MANIFEST_MEDIA_TYPE)?;
        Ok(WorkspaceSnapshot::new(metadata.hash, metadata.length))
    }

    /// Loads, integrity-checks, canonicalizes, and validates a snapshot manifest.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, corrupt, noncanonical, oversized, or unsupported manifests.
    pub fn manifest(
        &self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<WorkspaceManifest, WorkspaceError> {
        let encoded = self.blobs.read(&snapshot.manifest_hash)?;
        let actual_length = u64::try_from(encoded.len()).unwrap_or(u64::MAX);
        if actual_length != snapshot.manifest_length {
            return Err(WorkspaceError::ManifestLengthMismatch {
                expected: snapshot.manifest_length,
                actual: actual_length,
            });
        }
        enforce_limit(
            LimitKind::ManifestBytes,
            actual_length,
            self.limits.max_manifest_bytes,
        )?;
        let manifest: WorkspaceManifest = serde_json::from_slice(&encoded)?;
        if manifest.schema_version > WORKSPACE_SCHEMA_VERSION {
            return Err(WorkspaceError::FutureSchema {
                found: manifest.schema_version,
                maximum: WORKSPACE_SCHEMA_VERSION,
            });
        }
        if manifest.schema_version != WORKSPACE_SCHEMA_VERSION {
            return Err(WorkspaceError::UnsupportedSchema {
                found: manifest.schema_version,
            });
        }
        if serde_json::to_vec(&manifest)? != encoded {
            return Err(WorkspaceError::NonCanonicalManifest);
        }
        validate_manifest(&manifest, self.limits)?;
        Ok(manifest)
    }

    /// Validates the manifest and every referenced file blob without mutating a restore target.
    ///
    /// # Errors
    ///
    /// Returns an error for any invalid manifest, missing/corrupt blob, or length mismatch.
    pub fn validate(&self, snapshot: &WorkspaceSnapshot) -> Result<RestoreReport, WorkspaceError> {
        let manifest = self.manifest(snapshot)?;
        let prepared = self.prepare_files(&manifest)?;
        Ok(RestoreReport {
            entries: manifest.entries.len(),
            total_file_bytes: prepared
                .values()
                .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
                .sum(),
        })
    }

    /// Restores a snapshot into a new target directory.
    ///
    /// All referenced blobs are loaded and verified before a staging directory is created.
    ///
    /// # Errors
    ///
    /// Returns an error for validation, integrity, target, platform, or filesystem failure.
    pub fn restore(
        &self,
        snapshot: &WorkspaceSnapshot,
        target: impl AsRef<Path>,
    ) -> Result<RestoreReport, WorkspaceError> {
        self.restore_with_fault(snapshot, target, RestoreFault::None)
    }

    /// Restores with deterministic fault injection for cleanup tests.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::restore`] plus [`WorkspaceError::FaultInjected`].
    pub fn restore_with_fault(
        &self,
        snapshot: &WorkspaceSnapshot,
        target: impl AsRef<Path>,
        fault: RestoreFault,
    ) -> Result<RestoreReport, WorkspaceError> {
        let manifest = self.manifest(snapshot)?;
        let prepared = self.prepare_files(&manifest)?;
        let target = target.as_ref();
        let parent = validate_restore_target(target)?;
        let _restore_lock = RestoreLock::acquire(&parent, target)?;
        if target.try_exists()? {
            return Err(WorkspaceError::TargetExists {
                path: target.to_path_buf(),
            });
        }
        let staging_name = format!(
            ".epoch-restore-{}-{}",
            target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("workspace"),
            Uuid::new_v4()
        );
        let staging = parent.join(staging_name);
        create_private_directory(&staging)?;
        let mut guard = StagingGuard::new(staging.clone());
        if fault == RestoreFault::AfterStagingCreated {
            return Err(WorkspaceError::FaultInjected { point: fault });
        }

        let mut created_entries = 0;
        let mut directories = manifest
            .entries
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Directory { .. }))
            .collect::<Vec<_>>();
        directories.sort_by_key(|entry| (path_depth(&entry.path), entry.path.as_str()));
        for entry in &directories {
            let path = staging.join(manifest_path(&entry.path));
            create_private_directory(&path)?;
            created_entries += 1;
            maybe_fault_after_first(fault, created_entries)?;
        }

        for entry in &manifest.entries {
            let path = staging.join(manifest_path(&entry.path));
            match &entry.kind {
                EntryKind::Directory { .. } => continue,
                EntryKind::Regular { mode, .. } => {
                    let bytes =
                        prepared
                            .get(&entry.path)
                            .ok_or(WorkspaceError::InvalidManifest {
                                reason: "prepared file is missing",
                            })?;
                    write_new_file(&path, bytes, *mode)?;
                }
                EntryKind::Symlink { target } => create_symlink(target, &path)?,
            }
            created_entries += 1;
            maybe_fault_after_first(fault, created_entries)?;
        }

        sync_workspace_tree(&staging, &manifest)?;
        if fault == RestoreFault::BeforePublish {
            return Err(WorkspaceError::FaultInjected { point: fault });
        }
        if target.try_exists()? {
            return Err(WorkspaceError::TargetExists {
                path: target.to_path_buf(),
            });
        }
        let mut directory_handles = directories
            .iter()
            .map(|entry| File::open(staging.join(manifest_path(&entry.path))))
            .collect::<Result<Vec<_>, _>>()?;
        directory_handles.push(File::open(&staging)?);
        directories.sort_by_key(|entry| std::cmp::Reverse(path_depth(&entry.path)));
        for entry in directories {
            if let EntryKind::Directory { mode } = entry.kind {
                set_mode(&staging.join(manifest_path(&entry.path)), mode)?;
            }
        }
        set_mode(&staging, manifest.root_mode)?;
        for directory in directory_handles {
            directory.sync_all()?;
        }
        fs::rename(&staging, target).map_err(|error| {
            if target.exists() {
                WorkspaceError::TargetExists {
                    path: target.to_path_buf(),
                }
            } else {
                WorkspaceError::Io(error)
            }
        })?;
        sync_directory(&parent)?;
        guard.publish();
        Ok(RestoreReport {
            entries: manifest.entries.len(),
            total_file_bytes: prepared
                .values()
                .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
                .sum(),
        })
    }

    fn prepare_files(
        &self,
        manifest: &WorkspaceManifest,
    ) -> Result<BTreeMap<String, Vec<u8>>, WorkspaceError> {
        let mut files = BTreeMap::new();
        for entry in &manifest.entries {
            if let EntryKind::Regular {
                length,
                content_hash,
                ..
            } = &entry.kind
            {
                let bytes = self.blobs.read(content_hash)?;
                let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                if actual != *length {
                    return Err(WorkspaceError::ReferencedBlobLengthMismatch {
                        hash: content_hash.clone(),
                        expected: *length,
                        actual,
                    });
                }
                files.insert(entry.path.clone(), bytes);
            }
        }
        Ok(files)
    }
}

struct Capture<'a> {
    backend: &'a WorkspaceBackend,
    source: &'a Path,
    entries: Vec<WorkspaceEntry>,
    total_bytes: u64,
}

impl Capture<'_> {
    fn walk(&mut self, relative: &Path, depth: usize) -> Result<(), WorkspaceError> {
        let directory = self.source.join(relative);
        let mut children = fs::read_dir(&directory)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        children.sort();
        for name in children {
            if name == ".epoch" {
                continue;
            }
            let name = name
                .to_str()
                .ok_or(WorkspaceError::Unsupported(Unsupported::NonUtf8Path))?;
            let child_relative = relative.join(name);
            let manifest_path = path_to_manifest(&child_relative)?;
            enforce_path_limits(&manifest_path, depth + 1, self.backend.limits)?;
            let absolute = self.source.join(&child_relative);
            if absolute == self.backend.blob_root {
                continue;
            }
            let metadata = fs::symlink_metadata(&absolute)?;
            self.add_entry()?;
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                self.entries.push(WorkspaceEntry {
                    path: manifest_path,
                    kind: EntryKind::Directory {
                        mode: metadata_mode(&metadata, true),
                    },
                });
                self.walk(&child_relative, depth + 1)?;
            } else if file_type.is_symlink() {
                let target = fs::read_link(&absolute)?;
                let target = target.to_str().ok_or(WorkspaceError::Unsupported(
                    Unsupported::NonUtf8SymlinkTarget,
                ))?;
                validate_symlink_target(&manifest_path, target, self.backend.limits)?;
                self.entries.push(WorkspaceEntry {
                    path: manifest_path,
                    kind: EntryKind::Symlink {
                        target: target.to_owned(),
                    },
                });
            } else if file_type.is_file() {
                let (bytes, opened_metadata) =
                    read_regular_file(&absolute, &metadata, self.backend.limits.max_file_bytes)?;
                let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                self.total_bytes =
                    self.total_bytes
                        .checked_add(length)
                        .ok_or(WorkspaceError::LimitExceeded {
                            kind: LimitKind::TotalBytes,
                            actual: u64::MAX,
                            maximum: self.backend.limits.max_total_bytes,
                        })?;
                enforce_limit(
                    LimitKind::TotalBytes,
                    self.total_bytes,
                    self.backend.limits.max_total_bytes,
                )?;
                let blob = self.backend.blobs.put(&bytes, FILE_MEDIA_TYPE)?;
                let mode = metadata_mode(&opened_metadata, false);
                self.entries.push(WorkspaceEntry {
                    path: manifest_path,
                    kind: EntryKind::Regular {
                        mode,
                        executable: mode & 0o111 != 0,
                        length: blob.length,
                        content_hash: blob.hash,
                    },
                });
            } else {
                return Err(WorkspaceError::Unsupported(Unsupported::SpecialFile {
                    path: manifest_path,
                    kind: special_file_kind(file_type),
                }));
            }
        }
        Ok(())
    }

    fn add_entry(&self) -> Result<(), WorkspaceError> {
        let actual = self.entries.len().saturating_add(1);
        if actual > self.backend.limits.max_entries {
            Err(WorkspaceError::LimitExceeded {
                kind: LimitKind::Entries,
                actual: u64::try_from(actual).unwrap_or(u64::MAX),
                maximum: u64::try_from(self.backend.limits.max_entries).unwrap_or(u64::MAX),
            })
        } else {
            Ok(())
        }
    }
}

fn validate_limits(limits: WorkspaceLimits) -> Result<(), WorkspaceError> {
    if limits.max_entries == 0
        || limits.max_total_bytes == 0
        || limits.max_file_bytes == 0
        || limits.max_manifest_bytes == 0
        || limits.max_depth == 0
        || limits.max_path_bytes == 0
        || limits.max_component_bytes == 0
        || limits.max_symlink_target_bytes == 0
        || limits.max_file_bytes > limits.max_total_bytes
    {
        Err(WorkspaceError::InvalidLimits)
    } else {
        Ok(())
    }
}

fn validate_manifest(
    manifest: &WorkspaceManifest,
    limits: WorkspaceLimits,
) -> Result<(), WorkspaceError> {
    validate_mode(manifest.root_mode)?;
    if manifest.entries.len() > limits.max_entries {
        return Err(WorkspaceError::LimitExceeded {
            kind: LimitKind::Entries,
            actual: u64::try_from(manifest.entries.len()).unwrap_or(u64::MAX),
            maximum: u64::try_from(limits.max_entries).unwrap_or(u64::MAX),
        });
    }
    let mut previous: Option<&str> = None;
    let mut kinds = BTreeMap::new();
    let mut total_bytes = 0_u64;
    for entry in &manifest.entries {
        let depth = validate_manifest_path(&entry.path, limits)?;
        if previous.is_some_and(|value| value >= entry.path.as_str()) {
            return Err(WorkspaceError::NonCanonicalManifest);
        }
        previous = Some(&entry.path);
        match &entry.kind {
            EntryKind::Directory { mode } => validate_mode(*mode)?,
            EntryKind::Regular {
                mode,
                executable,
                length,
                ..
            } => {
                validate_mode(*mode)?;
                if *executable != (*mode & 0o111 != 0) {
                    return Err(WorkspaceError::InvalidManifest {
                        reason: "executable bit disagrees with mode",
                    });
                }
                enforce_limit(LimitKind::FileBytes, *length, limits.max_file_bytes)?;
                total_bytes =
                    total_bytes
                        .checked_add(*length)
                        .ok_or(WorkspaceError::LimitExceeded {
                            kind: LimitKind::TotalBytes,
                            actual: u64::MAX,
                            maximum: limits.max_total_bytes,
                        })?;
            }
            EntryKind::Symlink { target } => {
                validate_symlink_target(&entry.path, target, limits)?;
            }
        }
        enforce_limit(LimitKind::Depth, depth as u64, limits.max_depth as u64)?;
        kinds.insert(entry.path.as_str(), &entry.kind);
    }
    enforce_limit(LimitKind::TotalBytes, total_bytes, limits.max_total_bytes)?;
    for path in kinds.keys() {
        let components = path.split('/').collect::<Vec<_>>();
        for end in 1..components.len() {
            let parent = components[..end].join("/");
            if !matches!(
                kinds.get(parent.as_str()),
                Some(EntryKind::Directory { .. })
            ) {
                return Err(WorkspaceError::InvalidManifest {
                    reason: "entry parent is absent or not a directory",
                });
            }
        }
    }
    Ok(())
}

fn validate_manifest_path(path: &str, limits: WorkspaceLimits) -> Result<usize, WorkspaceError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with("//")
        || path.contains('\\')
        || path.as_bytes().get(1) == Some(&b':')
    {
        return Err(WorkspaceError::UnsafeManifestPath {
            path: path.to_owned(),
        });
    }
    let components = path.split('/').collect::<Vec<_>>();
    if components.iter().any(|component| {
        component.is_empty()
            || *component == "."
            || *component == ".."
            || *component == ".epoch"
            || component.as_bytes().contains(&0)
    }) {
        return Err(WorkspaceError::UnsafeManifestPath {
            path: path.to_owned(),
        });
    }
    enforce_path_limits(path, components.len(), limits)?;
    Ok(components.len())
}

fn enforce_path_limits(
    path: &str,
    depth: usize,
    limits: WorkspaceLimits,
) -> Result<(), WorkspaceError> {
    enforce_limit(
        LimitKind::PathBytes,
        u64::try_from(path.len()).unwrap_or(u64::MAX),
        u64::try_from(limits.max_path_bytes).unwrap_or(u64::MAX),
    )?;
    enforce_limit(
        LimitKind::Depth,
        u64::try_from(depth).unwrap_or(u64::MAX),
        u64::try_from(limits.max_depth).unwrap_or(u64::MAX),
    )?;
    let longest_component = path.split('/').map(str::len).max().unwrap_or(0);
    enforce_limit(
        LimitKind::ComponentBytes,
        u64::try_from(longest_component).unwrap_or(u64::MAX),
        u64::try_from(limits.max_component_bytes).unwrap_or(u64::MAX),
    )
}

fn validate_symlink_target(
    link_path: &str,
    target: &str,
    limits: WorkspaceLimits,
) -> Result<(), WorkspaceError> {
    enforce_limit(
        LimitKind::SymlinkTargetBytes,
        u64::try_from(target.len()).unwrap_or(u64::MAX),
        u64::try_from(limits.max_symlink_target_bytes).unwrap_or(u64::MAX),
    )?;
    if target.is_empty()
        || target.starts_with('/')
        || target.starts_with("//")
        || target.contains('\\')
        || target.as_bytes().get(1) == Some(&b':')
        || target.as_bytes().contains(&0)
    {
        return Err(WorkspaceError::Unsupported(Unsupported::SymlinkEscape {
            path: link_path.to_owned(),
        }));
    }
    let mut resolved = link_path
        .split('/')
        .take(link_path.split('/').count().saturating_sub(1))
        .collect::<Vec<_>>();
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if resolved.pop().is_none() {
                    return Err(WorkspaceError::Unsupported(Unsupported::SymlinkEscape {
                        path: link_path.to_owned(),
                    }));
                }
            }
            ".epoch" => {
                return Err(WorkspaceError::Unsupported(Unsupported::SymlinkEscape {
                    path: link_path.to_owned(),
                }));
            }
            _ => resolved.push(component),
        }
    }
    Ok(())
}

fn validate_mode(mode: u32) -> Result<(), WorkspaceError> {
    if mode & !0o777 == 0 {
        Ok(())
    } else {
        Err(WorkspaceError::InvalidManifest {
            reason: "mode contains unsupported bits",
        })
    }
}

fn enforce_limit(kind: LimitKind, actual: u64, maximum: u64) -> Result<(), WorkspaceError> {
    if actual > maximum {
        Err(WorkspaceError::LimitExceeded {
            kind,
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn path_to_manifest(path: &Path) -> Result<String, WorkspaceError> {
    let mut output = String::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(WorkspaceError::UnsafeManifestPath {
                path: path.display().to_string(),
            });
        };
        let component = component
            .to_str()
            .ok_or(WorkspaceError::Unsupported(Unsupported::NonUtf8Path))?;
        if !output.is_empty() {
            output.push('/');
        }
        output.push_str(component);
    }
    Ok(output)
}

fn manifest_path(path: &str) -> PathBuf {
    path.split('/').collect()
}

fn path_depth(path: &str) -> usize {
    path.split('/').count()
}

fn read_regular_file(
    path: &Path,
    observed: &fs::Metadata,
    maximum: u64,
) -> Result<(Vec<u8>, fs::Metadata), WorkspaceError> {
    enforce_limit(LimitKind::FileBytes, observed.len(), maximum)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || !same_file(observed, &opened) {
        return Err(WorkspaceError::SourceChanged { path: path.into() });
    }
    let mut bytes = Vec::with_capacity(usize::try_from(opened.len()).map_err(|_| {
        WorkspaceError::LimitExceeded {
            kind: LimitKind::FileBytes,
            actual: opened.len(),
            maximum,
        }
    })?);
    Read::by_ref(&mut file)
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    enforce_limit(LimitKind::FileBytes, actual, maximum)?;
    let after = file.metadata()?;
    if !same_file(&opened, &after) || after.len() != actual {
        return Err(WorkspaceError::SourceChanged { path: path.into() });
    }
    Ok((bytes, after))
}

#[cfg(unix)]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
}

#[cfg(not(unix))]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

#[cfg(unix)]
fn metadata_mode(metadata: &fs::Metadata, _directory: bool) -> u32 {
    metadata.mode() & 0o777
}

#[cfg(not(unix))]
fn metadata_mode(metadata: &fs::Metadata, directory: bool) -> u32 {
    if directory {
        0o755
    } else if metadata.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

#[cfg(unix)]
fn special_file_kind(file_type: fs::FileType) -> &'static str {
    if file_type.is_fifo() {
        "fifo"
    } else if file_type.is_socket() {
        "socket"
    } else if file_type.is_block_device() {
        "block_device"
    } else if file_type.is_char_device() {
        "character_device"
    } else {
        "unknown"
    }
}

#[cfg(not(unix))]
fn special_file_kind(_file_type: fs::FileType) -> &'static str {
    "unsupported"
}

fn validate_restore_target(target: &Path) -> Result<PathBuf, WorkspaceError> {
    if target.file_name().is_none()
        || target
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(WorkspaceError::UnsafeRestoreTarget {
            path: target.to_path_buf(),
        });
    }
    if target.try_exists()? {
        return Err(WorkspaceError::TargetExists {
            path: target.to_path_buf(),
        });
    }
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let metadata = fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(WorkspaceError::UnsafeRestoreTarget {
            path: target.to_path_buf(),
        });
    }
    Ok(parent.to_path_buf())
}

fn create_private_directory(path: &Path) -> Result<(), WorkspaceError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8], mode: u32) -> Result<(), WorkspaceError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    set_mode(path, mode)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), WorkspaceError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(path: &Path, mode: u32) -> Result<(), WorkspaceError> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(mode & 0o200 == 0);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &str, link: &Path) -> Result<(), WorkspaceError> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_symlink(_target: &str, _link: &Path) -> Result<(), WorkspaceError> {
    Err(WorkspaceError::Unsupported(Unsupported::SymlinksOnPlatform))
}

fn maybe_fault_after_first(fault: RestoreFault, created: usize) -> Result<(), WorkspaceError> {
    if fault == RestoreFault::AfterFirstEntry && created == 1 {
        Err(WorkspaceError::FaultInjected { point: fault })
    } else {
        Ok(())
    }
}

fn sync_workspace_tree(root: &Path, manifest: &WorkspaceManifest) -> Result<(), WorkspaceError> {
    let mut directories = manifest
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, EntryKind::Directory { .. }))
        .map(|entry| root.join(manifest_path(&entry.path)))
        .collect::<Vec<_>>();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        sync_directory(&directory)?;
    }
    sync_directory(root)
}

fn sync_directory(path: &Path) -> Result<(), WorkspaceError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

struct StagingGuard {
    path: PathBuf,
    published: bool,
}

struct RestoreLock {
    file: File,
    path: PathBuf,
    parent: PathBuf,
}

impl RestoreLock {
    fn acquire(parent: &Path, target: &Path) -> Result<Self, WorkspaceError> {
        let target_hash = BlobHash::digest(target.as_os_str().as_encoded_bytes());
        let path = parent.join(format!(".epoch-restore-lock-{target_hash}"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(WorkspaceError::TargetExists {
                    path: target.to_path_buf(),
                });
            }
            Err(error) => return Err(error.into()),
        };
        let mut lock = Self {
            file,
            path,
            parent: parent.to_path_buf(),
        };
        lock.file.write_all(b"epoch workspace restore lock\n")?;
        lock.file.sync_all()?;
        sync_directory(parent)?;
        Ok(lock)
    }
}

impl Drop for RestoreLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = sync_directory(&self.parent);
    }
}

impl StagingGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            published: false,
        }
    }

    fn publish(&mut self) {
        self.published = true;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if !self.published {
            let _ = make_tree_removable(&self.path);
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn make_tree_removable(path: &Path) -> Result<(), std::io::Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        make_directory_owner_accessible(path)?;
        for entry in fs::read_dir(path)? {
            make_tree_removable(&entry?.path())?;
        }
    } else {
        make_file_owner_writable(path, metadata.permissions())?;
    }
    Ok(())
}

#[cfg(unix)]
fn make_directory_owner_accessible(path: &Path) -> Result<(), std::io::Error> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn make_directory_owner_accessible(path: &Path) -> Result<(), std::io::Error> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)
}

#[cfg(unix)]
fn make_file_owner_writable(
    path: &Path,
    _permissions: fs::Permissions,
) -> Result<(), std::io::Error> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn make_file_owner_writable(
    path: &Path,
    mut permissions: fs::Permissions,
) -> Result<(), std::io::Error> {
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace source root must be a real directory")]
    InvalidSourceRoot,
    #[error("workspace source is equal to or contained by the managed state root")]
    StateRootOverlapsWorkspace,
    #[error("invalid workspace limits")]
    InvalidLimits,
    #[error("workspace limit {kind:?} exceeded: {actual} > {maximum}")]
    LimitExceeded {
        kind: LimitKind,
        actual: u64,
        maximum: u64,
    },
    #[error("workspace feature is unsupported: {0:?}")]
    Unsupported(Unsupported),
    #[error("unsafe manifest path {path:?}")]
    UnsafeManifestPath { path: String },
    #[error("unsafe restore target {path:?}")]
    UnsafeRestoreTarget { path: PathBuf },
    #[error("restore target already exists: {path:?}")]
    TargetExists { path: PathBuf },
    #[error("source changed while capturing {path:?}")]
    SourceChanged { path: PathBuf },
    #[error("future workspace schema {found}; maximum supported is {maximum}")]
    FutureSchema { found: u32, maximum: u32 },
    #[error("unsupported workspace schema {found}")]
    UnsupportedSchema { found: u32 },
    #[error("workspace manifest is not canonical")]
    NonCanonicalManifest,
    #[error("invalid workspace manifest: {reason}")]
    InvalidManifest { reason: &'static str },
    #[error("manifest length mismatch: expected {expected}, read {actual}")]
    ManifestLengthMismatch { expected: u64, actual: u64 },
    #[error("referenced blob {hash} length mismatch: expected {expected}, read {actual}")]
    ReferencedBlobLengthMismatch {
        hash: BlobHash,
        expected: u64,
        actual: u64,
    },
    #[error("injected workspace restore fault at {point:?}")]
    FaultInjected { point: RestoreFault },
    #[error(transparent)]
    Blob(#[from] BlobError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
