use std::{
    fs::{self, FileTimes, OpenOptions},
    str::FromStr,
    sync::Arc,
    thread,
    time::{Duration, SystemTime},
};

use epoch_blob::{BlobError, BlobHash, BlobStore};
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

fn store() -> (TempDir, BlobStore) {
    let directory = TempDir::new().expect("create test directory");
    let store = BlobStore::open(directory.path().join("blobs")).expect("open blob store");
    (directory, store)
}

#[test]
fn put_returns_hash_length_and_media_type_and_read_verifies_content() {
    let (_directory, store) = store();
    let bytes = b"an event payload";

    let metadata = store.put(bytes, "application/json").expect("persist blob");

    assert_eq!(metadata.hash, BlobHash::digest(bytes));
    assert_eq!(metadata.length, bytes.len() as u64);
    assert_eq!(metadata.media_type, "application/json");
    assert_eq!(store.read(&metadata.hash).expect("read blob"), bytes);
}

#[test]
fn duplicate_content_has_one_canonical_file() {
    let (_directory, store) = store();
    let bytes = b"same content";

    let first = store.put(bytes, "text/plain").expect("first write");
    let second = store.put(bytes, "text/plain").expect("duplicate write");

    assert_eq!(second, first);
    let shard = store
        .blob_path(&first.hash)
        .parent()
        .expect("blob has shard directory")
        .to_path_buf();
    let final_files = fs::read_dir(shard)
        .expect("read shard")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect shard files");
    assert_eq!(
        final_files.len(),
        2,
        "one payload and one media-type record should be canonical"
    );
}

#[test]
fn duplicate_content_rejects_a_conflicting_media_type_even_after_reopen() {
    let directory = TempDir::new().expect("create test directory");
    let root = directory.path().join("blobs");
    let store = BlobStore::open(&root).expect("open blob store");
    let bytes = b"typed content";
    let original = store.put(bytes, "text/plain").expect("first write");
    assert_eq!(
        store.put(bytes, "text/plain").expect("idempotent write"),
        original
    );
    assert!(matches!(
        store.put(bytes, "application/json"),
        Err(BlobError::MediaTypeConflict {
            hash,
            stored,
            requested,
        }) if hash == original.hash && stored == "text/plain" && requested == "application/json"
    ));
    drop(store);

    let reopened = BlobStore::open(root).expect("reopen blob store");
    assert!(matches!(
        reopened.put(bytes, "application/json"),
        Err(BlobError::MediaTypeConflict { hash, .. }) if hash == original.hash
    ));
}

#[test]
fn rejects_unbounded_or_control_character_media_types_before_writing() {
    let (_directory, store) = store();
    let bytes = b"not published";
    for invalid in [
        String::new(),
        "text/plain\nsecret".to_owned(),
        "x".repeat(256),
    ] {
        assert!(matches!(
            store.put(bytes, invalid),
            Err(BlobError::InvalidMediaType { .. })
        ));
    }
    assert!(!store.blob_path(&BlobHash::digest(bytes)).exists());
}

#[test]
fn concurrent_conflicting_media_types_choose_exactly_one_canonical_value() {
    let (_directory, store) = store();
    let store = Arc::new(store);
    let media_types = ["text/plain", "application/json"];
    let handles = media_types.map(|media_type| {
        let store = Arc::clone(&store);
        thread::spawn(move || store.put(b"racing typed content", media_type))
    });
    let results = handles.map(|handle| handle.join().expect("writer did not panic"));

    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(BlobError::MediaTypeConflict { .. })))
            .count(),
        1
    );
    let winner = results
        .into_iter()
        .find_map(Result::ok)
        .expect("one media type wins");
    assert_eq!(
        store
            .put(b"racing typed content", &winner.media_type)
            .expect("winner remains idempotent"),
        winner
    );
}

#[test]
fn configured_size_limit_bounds_put_and_read() {
    let directory = TempDir::new().expect("create test directory");
    let store = BlobStore::open_with_max_blob_bytes(directory.path().join("blobs"), 4)
        .expect("open bounded store");
    let exact = store.put(b"four", "text/plain").expect("write at limit");
    assert_eq!(store.read(&exact.hash).expect("read at limit"), b"four");

    let oversized = b"oversized";
    assert!(matches!(
        store.put(oversized, "text/plain"),
        Err(BlobError::SizeLimitExceeded {
            actual: 9,
            maximum: 4
        })
    ));
    assert!(!store.blob_path(&BlobHash::digest(oversized)).exists());

    fs::write(store.blob_path(&exact.hash), b"grown").expect("grow canonical fixture");
    assert!(matches!(
        store.read(&exact.hash),
        Err(BlobError::SizeLimitExceeded {
            actual: 5,
            maximum: 4
        })
    ));
}

#[test]
fn concurrent_duplicate_writers_publish_one_verified_blob() {
    let (_directory, store) = store();
    let store = Arc::new(store);
    let handles = (0..8)
        .map(|_| {
            let store = Arc::clone(&store);
            thread::spawn(move || store.put(b"racing content", "text/plain"))
        })
        .collect::<Vec<_>>();

    let metadata = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .expect("writer did not panic")
                .expect("writer succeeded")
        })
        .collect::<Vec<_>>();
    assert!(metadata.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(
        store.read(&metadata[0].hash).expect("read raced blob"),
        b"racing content"
    );
}

#[test]
fn tampered_blob_is_rejected_on_read_and_duplicate_put() {
    let (_directory, store) = store();
    let bytes = b"trusted bytes";
    let metadata = store.put(bytes, "text/plain").expect("persist blob");
    fs::write(store.blob_path(&metadata.hash), b"tampered bytes").expect("tamper fixture");

    assert!(matches!(
        store.read(&metadata.hash),
        Err(BlobError::HashMismatch { expected, .. }) if expected == metadata.hash
    ));
    assert!(matches!(
        store.put(bytes, "text/plain"),
        Err(BlobError::HashMismatch { expected, .. }) if expected == metadata.hash
    ));
}

#[test]
fn interrupted_temp_write_never_appears_as_a_valid_blob() {
    let (directory, store) = store();
    let complete = b"complete blob";
    let hash = BlobHash::digest(complete);
    let temp_directory = directory.path().join("blobs/sha256/.tmp");
    fs::write(temp_directory.join(format!("{hash}.partial")), b"complete")
        .expect("write partial temp blob");

    assert!(matches!(store.read(&hash), Err(BlobError::NotFound(found)) if found == hash));
    assert!(!store.blob_path(&hash).exists());
}

fn managed_temp_name(bytes: &[u8]) -> String {
    format!("{}.{}.tmp", BlobHash::digest(bytes), uuid::Uuid::new_v4())
}

#[test]
fn explicit_cleanup_removes_only_managed_temporary_files() {
    let (directory, store) = store();
    let temp_directory = directory.path().join("blobs/sha256/.tmp");
    let managed = temp_directory.join(managed_temp_name(b"managed"));
    let unrelated = temp_directory.join("operator-note.txt");
    fs::write(&managed, b"partial").expect("write managed temporary file");
    fs::write(&unrelated, b"keep").expect("write unrelated file");

    assert_eq!(
        store
            .cleanup_stale_temporary_files(Duration::ZERO)
            .expect("clean stale files"),
        1
    );
    assert!(!managed.exists());
    assert_eq!(fs::read(unrelated).expect("read unrelated file"), b"keep");
}

#[test]
fn opening_a_store_cleans_only_old_managed_temporary_files() {
    let directory = TempDir::new().expect("create test directory");
    let root = directory.path().join("blobs");
    let store = BlobStore::open(&root).expect("open blob store");
    let temp_directory = root.join("sha256/.tmp");
    let stale = temp_directory.join(managed_temp_name(b"stale"));
    let fresh = temp_directory.join(managed_temp_name(b"fresh"));
    fs::write(&stale, b"stale").expect("write stale file");
    fs::write(&fresh, b"fresh").expect("write fresh file");
    let old = SystemTime::now()
        .checked_sub(Duration::from_secs(48 * 60 * 60))
        .expect("represent old timestamp");
    OpenOptions::new()
        .write(true)
        .open(&stale)
        .expect("open stale file")
        .set_times(FileTimes::new().set_modified(old))
        .expect("age stale file");
    drop(store);

    BlobStore::open(&root).expect("reopen blob store");

    assert!(
        !stale.exists(),
        "old managed temporary file should be removed"
    );
    assert!(
        fresh.exists(),
        "a possibly live temporary file must be retained"
    );
}

#[cfg(unix)]
#[test]
fn temporary_cleanup_rejects_symlinks_without_touching_the_target() {
    let (directory, store) = store();
    let target = directory.path().join("target");
    fs::write(&target, b"do not delete").expect("write target");
    let temp_link = directory
        .path()
        .join("blobs/sha256/.tmp")
        .join(managed_temp_name(b"link"));
    symlink(&target, &temp_link).expect("create temporary symlink");

    assert!(matches!(
        store.cleanup_stale_temporary_files(Duration::ZERO),
        Err(BlobError::UnsafeSymlink { .. })
    ));
    assert_eq!(fs::read(target).expect("read target"), b"do not delete");
    assert!(fs::symlink_metadata(temp_link).is_ok());
}

#[test]
fn blob_hash_parser_accepts_only_canonical_lowercase_sha256() {
    let canonical = BlobHash::digest(b"payload").to_string();
    assert_eq!(
        BlobHash::from_str(&canonical)
            .expect("valid hash")
            .to_string(),
        canonical
    );

    for invalid in [
        "",
        "abc",
        &"A".repeat(64),
        &"g".repeat(64),
        &format!("{}x", "a".repeat(64)),
    ] {
        assert!(
            BlobHash::from_str(invalid).is_err(),
            "{invalid:?} should fail"
        );
    }
}

#[test]
fn deserialization_cannot_bypass_blob_hash_validation() {
    let uppercase = format!("\"{}\"", "A".repeat(64));
    assert!(serde_json::from_str::<BlobHash>(&uppercase).is_err());

    let canonical = BlobHash::digest(b"payload");
    let encoded = serde_json::to_string(&canonical).expect("serialize hash");
    assert_eq!(
        serde_json::from_str::<BlobHash>(&encoded).expect("deserialize valid hash"),
        canonical
    );
}

#[cfg(unix)]
fn unix_mode(path: &std::path::Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("read path metadata")
        .mode()
        & 0o7777
}

#[cfg(unix)]
#[test]
fn creates_private_directories_and_blob_files() {
    let (directory, store) = store();
    let metadata = store
        .put(b"private payload", "application/octet-stream")
        .expect("persist private blob");
    let root = directory.path().join("blobs");
    let sha256 = root.join("sha256");
    let temp = sha256.join(".tmp");
    let blob_path = store.blob_path(&metadata.hash);
    let shard = blob_path.parent().expect("blob has shard directory");

    for path in [&root, &sha256, &temp, shard] {
        assert_eq!(unix_mode(path), 0o700, "{} must be private", path.display());
    }
    let blob_path = store.blob_path(&metadata.hash);
    let media_type_path = blob_path.with_file_name(format!("{}.media-type", metadata.hash));
    assert_eq!(unix_mode(&blob_path), 0o600);
    assert_eq!(unix_mode(&media_type_path), 0o600);
}

#[cfg(unix)]
#[test]
fn rejects_an_insecure_existing_root_instead_of_silently_chmodding_it() {
    let directory = TempDir::new().expect("create test directory");
    let root = directory.path().join("blobs");
    fs::create_dir(&root).expect("create blob root");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755))
        .expect("make root intentionally insecure");

    assert!(matches!(
        BlobStore::open(&root),
        Err(BlobError::InsecurePermissions {
            expected: 0o700,
            actual: 0o755,
            ..
        })
    ));
    assert_eq!(unix_mode(&root), 0o755, "open must not hide unsafe state");
}

#[cfg(unix)]
#[test]
fn rejects_a_symlink_as_the_blob_root_without_touching_its_target() {
    let directory = TempDir::new().expect("create test directory");
    let target = directory.path().join("target");
    fs::create_dir(&target).expect("create symlink target");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).expect("secure symlink target");
    let root = directory.path().join("blobs");
    symlink(&target, &root).expect("create root symlink");

    assert!(matches!(
        BlobStore::open(&root),
        Err(BlobError::UnsafeSymlink { .. })
    ));
    assert!(!target.join("sha256").exists());
}

#[cfg(unix)]
#[test]
fn rejects_symlinks_inside_the_managed_store() {
    let directory = TempDir::new().expect("create test directory");
    let root = directory.path().join("blobs");
    fs::create_dir(&root).expect("create blob root");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("secure blob root");
    let target = directory.path().join("target");
    fs::create_dir(&target).expect("create symlink target");
    symlink(&target, root.join("sha256")).expect("create managed symlink");

    assert!(matches!(
        BlobStore::open(&root),
        Err(BlobError::UnsafeSymlink { .. })
    ));
    assert!(!target.join(".tmp").exists());
}

#[cfg(unix)]
#[test]
fn read_rejects_a_symlink_at_the_canonical_blob_path() {
    let (directory, store) = store();
    let content = b"outside payload";
    let hash = BlobHash::digest(content);
    let blob_path = store.blob_path(&hash);
    let shard = blob_path.parent().expect("blob has shard");
    fs::create_dir(shard).expect("create shard");
    fs::set_permissions(shard, fs::Permissions::from_mode(0o700)).expect("secure shard");
    let outside = directory.path().join("outside");
    fs::write(&outside, content).expect("write outside file");
    symlink(&outside, &blob_path).expect("create blob symlink");

    assert!(matches!(
        store.read(&hash),
        Err(BlobError::UnsafeSymlink { .. })
    ));
}
