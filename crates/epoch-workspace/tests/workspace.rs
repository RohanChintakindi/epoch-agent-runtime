use std::{fs, path::Path};

#[cfg(unix)]
use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};

use epoch_blob::BlobStore;
use epoch_workspace::{
    EntryKind, HardlinkPolicy, LimitKind, RestoreFault, Unsupported, WorkspaceBackend,
    WorkspaceError, WorkspaceLimits, WorkspaceSnapshot,
};
use serde_json::Value;
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    source: std::path::PathBuf,
    blobs: std::path::PathBuf,
    restore_parent: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().expect("temp fixture");
        let source = temp.path().join("source");
        let blobs = temp.path().join("blobs");
        let restore_parent = temp.path().join("restores");
        fs::create_dir(&source).expect("source");
        fs::create_dir(&restore_parent).expect("restore parent");
        Self {
            _temp: temp,
            source,
            blobs,
            restore_parent,
        }
    }

    fn backend(&self) -> WorkspaceBackend {
        WorkspaceBackend::open(&self.blobs, WorkspaceLimits::default()).expect("backend")
    }

    fn target(&self, name: &str) -> std::path::PathBuf {
        self.restore_parent.join(name)
    }
}

fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parents");
    }
    fs::write(path, bytes).expect("write fixture");
}

#[cfg(unix)]
fn symlink(target: &str, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("symlink fixture");
}

#[test]
#[cfg(unix)]
fn snapshot_restore_preserves_tree_bytes_modes_empty_files_and_symlink_text() {
    let fixture = Fixture::new();
    write(&fixture.source.join("README.md"), b"hello\n");
    write(&fixture.source.join("bin/run"), b"#!/bin/sh\nexit 0\n");
    write(&fixture.source.join("data/binary"), &[0, 0xff, 1, 0x80]);
    write(&fixture.source.join("empty"), b"");
    fs::create_dir_all(fixture.source.join("empty-dir")).expect("empty dir");
    fs::set_permissions(
        fixture.source.join("bin/run"),
        fs::Permissions::from_mode(0o751),
    )
    .expect("mode");
    symlink("../README.md", &fixture.source.join("data/readme-link"));

    let backend = fixture.backend();
    let snapshot = backend.snapshot(&fixture.source).expect("snapshot");
    let target = fixture.target("restored");
    let report = backend.restore(&snapshot, &target).expect("restore");

    assert_eq!(
        fs::read(target.join("README.md")).expect("read"),
        b"hello\n"
    );
    assert_eq!(
        fs::read(target.join("data/binary")).expect("binary"),
        [0, 0xff, 1, 0x80]
    );
    assert_eq!(fs::read(target.join("empty")).expect("empty"), b"");
    assert!(target.join("empty-dir").is_dir());
    assert_eq!(
        fs::read_link(target.join("data/readme-link")).expect("link"),
        Path::new("../README.md")
    );
    assert_eq!(
        fs::metadata(target.join("bin/run"))
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    assert!(report.entries >= 7);
}

#[test]
fn snapshot_is_stable_and_immutable_after_source_mutation() {
    let fixture = Fixture::new();
    write(&fixture.source.join("z"), b"last");
    write(&fixture.source.join("a/nested"), b"first");
    let backend = fixture.backend();
    let first = backend.snapshot(&fixture.source).expect("first");
    let second = backend.snapshot(&fixture.source).expect("second");
    assert_eq!(first, second);

    write(&fixture.source.join("a/nested"), b"mutated");
    fs::remove_file(fixture.source.join("z")).expect("remove");
    write(&fixture.source.join("new"), b"new");
    let target = fixture.target("original");
    backend.restore(&first, &target).expect("restore original");
    assert_eq!(fs::read(target.join("a/nested")).expect("nested"), b"first");
    assert_eq!(fs::read(target.join("z")).expect("z"), b"last");
    assert!(!target.join("new").exists());
}

#[test]
#[cfg(unix)]
fn restore_preserves_workspace_root_mode() {
    let fixture = Fixture::new();
    fs::set_permissions(&fixture.source, fs::Permissions::from_mode(0o711)).expect("root mode");
    write(&fixture.source.join("file"), b"content");
    let backend = fixture.backend();
    let snapshot = backend.snapshot(&fixture.source).expect("snapshot");
    let target = fixture.target("root-mode");
    backend.restore(&snapshot, &target).expect("restore");
    assert_eq!(
        fs::metadata(target)
            .expect("root metadata")
            .permissions()
            .mode()
            & 0o777,
        0o711
    );
}

#[test]
#[cfg(unix)]
fn hardlinks_are_explicitly_materialized_as_independent_files() {
    use std::os::unix::fs::MetadataExt;

    let fixture = Fixture::new();
    write(&fixture.source.join("one"), b"shared");
    fs::hard_link(fixture.source.join("one"), fixture.source.join("two")).expect("hard link");
    let backend = fixture.backend();
    let snapshot = backend.snapshot(&fixture.source).expect("snapshot");
    let manifest = backend.manifest(&snapshot).expect("manifest");
    assert_eq!(manifest.hardlink_policy, HardlinkPolicy::Materialize);
    let target = fixture.target("materialized");
    backend.restore(&snapshot, &target).expect("restore");
    let one = fs::metadata(target.join("one")).expect("one");
    let two = fs::metadata(target.join("two")).expect("two");
    assert_ne!(one.ino(), two.ino());
}

#[test]
fn nested_epoch_state_is_never_captured() {
    let fixture = Fixture::new();
    let nested_blobs = fixture.source.join(".epoch/blobs");
    write(&fixture.source.join("kept"), b"yes");
    fs::create_dir(fixture.source.join(".epoch")).expect("state parent");
    let backend = WorkspaceBackend::open(&nested_blobs, WorkspaceLimits::default()).expect("open");
    let snapshot = backend.snapshot(&fixture.source).expect("snapshot");
    let manifest = backend.manifest(&snapshot).expect("manifest");
    assert!(manifest.entries.iter().any(|entry| entry.path == "kept"));
    assert!(
        manifest
            .entries
            .iter()
            .all(|entry| !entry.path.starts_with(".epoch"))
    );
}

#[test]
#[cfg(unix)]
fn symlink_escape_and_special_files_are_explicitly_unsupported() {
    let fixture = Fixture::new();
    symlink("../../outside", &fixture.source.join("escape"));
    let backend = fixture.backend();
    assert!(matches!(
        backend.snapshot(&fixture.source),
        Err(WorkspaceError::Unsupported(
            Unsupported::SymlinkEscape { .. }
        ))
    ));

    fs::remove_file(fixture.source.join("escape")).expect("remove link");
    let fifo = fixture.source.join("pipe");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo");
    assert!(status.success());
    assert!(matches!(
        backend.snapshot(&fixture.source),
        Err(WorkspaceError::Unsupported(Unsupported::SpecialFile { .. }))
    ));
}

#[test]
#[cfg(unix)]
fn non_utf8_names_are_rejected_without_lossy_normalization() {
    let fixture = Fixture::new();
    let name = std::ffi::OsString::from_vec(vec![b'f', 0x80]);
    if let Err(error) = fs::write(fixture.source.join(name), b"bytes") {
        #[cfg(target_os = "macos")]
        {
            assert_eq!(error.raw_os_error(), Some(92));
            return;
        }
        #[cfg(not(target_os = "macos"))]
        panic!("create non-UTF-8 fixture: {error}");
    }
    assert!(matches!(
        fixture.backend().snapshot(&fixture.source),
        Err(WorkspaceError::Unsupported(Unsupported::NonUtf8Path))
    ));
}

#[test]
fn configured_file_total_depth_and_path_limits_fail_before_manifest_commit() {
    let fixture = Fixture::new();
    write(&fixture.source.join("a/b/c"), b"12345");
    let limits = WorkspaceLimits {
        max_file_bytes: 4,
        ..WorkspaceLimits::default()
    };
    let backend = WorkspaceBackend::open(&fixture.blobs, limits).expect("backend");
    assert!(matches!(
        backend.snapshot(&fixture.source),
        Err(WorkspaceError::LimitExceeded {
            kind: LimitKind::FileBytes,
            ..
        })
    ));
}

#[test]
fn restore_validates_every_blob_before_creating_the_target() {
    let fixture = Fixture::new();
    write(&fixture.source.join("one"), b"one");
    write(&fixture.source.join("two"), b"two");
    let backend = fixture.backend();
    let snapshot = backend.snapshot(&fixture.source).expect("snapshot");
    let manifest = backend.manifest(&snapshot).expect("manifest");
    let missing = manifest
        .entries
        .iter()
        .find_map(|entry| match &entry.kind {
            EntryKind::Regular { content_hash, .. } => Some(content_hash),
            _ => None,
        })
        .expect("file hash");
    let blobs = BlobStore::open(&fixture.blobs).expect("blob store");
    fs::remove_file(blobs.blob_path(missing)).expect("remove content blob");

    let target = fixture.target("must-not-exist");
    assert!(backend.restore(&snapshot, &target).is_err());
    assert!(!target.exists());
    assert!(
        fs::read_dir(&fixture.restore_parent)
            .expect("parent")
            .next()
            .is_none()
    );
}

#[test]
fn corrupt_manifest_blob_is_rejected_before_target_or_staging_creation() {
    let fixture = Fixture::new();
    write(&fixture.source.join("file"), b"content");
    let backend = fixture.backend();
    let snapshot = backend.snapshot(&fixture.source).expect("snapshot");
    let blobs = BlobStore::open(&fixture.blobs).expect("blobs");
    fs::write(blobs.blob_path(snapshot.manifest_hash()), b"tampered").expect("tamper manifest");
    let target = fixture.target("corrupt");
    assert!(backend.restore(&snapshot, &target).is_err());
    assert!(!target.exists());
    assert!(fs::read_dir(&fixture.restore_parent)
        .expect("parent")
        .next()
        .is_none());
}

#[test]
fn future_noncanonical_and_wrong_length_manifests_are_rejected_before_mutation() {
    let fixture = Fixture::new();
    write(&fixture.source.join("file"), b"content");
    let backend = fixture.backend();
    let valid = backend.snapshot(&fixture.source).expect("snapshot");
    let manifest = backend.manifest(&valid).expect("manifest");
    let mut value = serde_json::to_value(&manifest).expect("value");
    value["schema_version"] = Value::from(999);
    let encoded = serde_json::to_vec(&value).expect("future bytes");
    let blobs = BlobStore::open(&fixture.blobs).expect("blobs");
    let metadata = blobs
        .put(&encoded, epoch_workspace::MANIFEST_MEDIA_TYPE)
        .expect("future blob");
    let future = WorkspaceSnapshot::new(metadata.hash, metadata.length);
    let target = fixture.target("future");
    assert!(matches!(
        backend.restore(&future, &target),
        Err(WorkspaceError::FutureSchema { found: 999, .. })
    ));
    assert!(!target.exists());

    let wrong = WorkspaceSnapshot::new(valid.manifest_hash().clone(), valid.manifest_length() + 1);
    assert!(matches!(
        backend.restore(&wrong, fixture.target("wrong-length")),
        Err(WorkspaceError::ManifestLengthMismatch { .. })
    ));

    let mut pretty = serde_json::to_vec_pretty(&manifest).expect("pretty");
    pretty.push(b'\n');
    let metadata = blobs
        .put(&pretty, epoch_workspace::MANIFEST_MEDIA_TYPE)
        .expect("pretty blob");
    let noncanonical = WorkspaceSnapshot::new(metadata.hash, metadata.length);
    assert!(matches!(
        backend.restore(&noncanonical, fixture.target("pretty")),
        Err(WorkspaceError::NonCanonicalManifest)
    ));
}

#[test]
fn traversal_manifest_and_existing_target_fail_closed_without_escape_or_clobber() {
    let fixture = Fixture::new();
    write(&fixture.source.join("file"), b"content");
    let backend = fixture.backend();
    let valid = backend.snapshot(&fixture.source).expect("snapshot");
    let mut manifest = backend.manifest(&valid).expect("manifest");
    manifest.entries[0].path = "../escaped".to_owned();
    let encoded = serde_json::to_vec(&manifest).expect("malicious manifest");
    let metadata = BlobStore::open(&fixture.blobs)
        .expect("blobs")
        .put(&encoded, epoch_workspace::MANIFEST_MEDIA_TYPE)
        .expect("manifest blob");
    let malicious = WorkspaceSnapshot::new(metadata.hash, metadata.length);
    assert!(matches!(
        backend.restore(&malicious, fixture.target("traversal")),
        Err(WorkspaceError::UnsafeManifestPath { .. })
    ));
    assert!(!fixture.restore_parent.join("escaped").exists());

    let existing = fixture.target("existing");
    fs::create_dir(&existing).expect("existing");
    write(&existing.join("sentinel"), b"keep");
    assert!(matches!(
        backend.restore(&valid, &existing),
        Err(WorkspaceError::TargetExists { .. })
    ));
    assert_eq!(
        fs::read(existing.join("sentinel")).expect("sentinel"),
        b"keep"
    );
}

#[test]
fn injected_restore_interrupts_remove_staging_and_never_publish_partial_target() {
    for fault in [
        RestoreFault::AfterStagingCreated,
        RestoreFault::AfterFirstEntry,
        RestoreFault::BeforePublish,
    ] {
        let fixture = Fixture::new();
        write(&fixture.source.join("dir/one"), b"one");
        write(&fixture.source.join("two"), b"two");
        let backend = fixture.backend();
        let snapshot = backend.snapshot(&fixture.source).expect("snapshot");
        let target = fixture.target("interrupted");
        assert!(matches!(
            backend.restore_with_fault(&snapshot, &target, fault),
            Err(WorkspaceError::FaultInjected { point }) if point == fault
        ));
        assert!(!target.exists());
        assert!(
            fs::read_dir(&fixture.restore_parent)
                .expect("parent")
                .next()
                .is_none()
        );
    }
}

#[test]
#[cfg(unix)]
fn interrupted_restore_cleans_staging_even_with_inaccessible_final_directory_mode() {
    let fixture = Fixture::new();
    write(&fixture.source.join("locked/file"), b"content");
    fs::set_permissions(
        fixture.source.join("locked"),
        fs::Permissions::from_mode(0o500),
    )
    .expect("restrict directory");
    let backend = fixture.backend();
    let snapshot = backend.snapshot(&fixture.source).expect("snapshot");
    let target = fixture.target("interrupted-locked");
    assert!(matches!(
        backend.restore_with_fault(&snapshot, &target, RestoreFault::BeforePublish),
        Err(WorkspaceError::FaultInjected {
            point: RestoreFault::BeforePublish
        })
    ));
    assert!(!target.exists());
    assert!(fs::read_dir(&fixture.restore_parent)
        .expect("parent")
        .next()
        .is_none());
}
