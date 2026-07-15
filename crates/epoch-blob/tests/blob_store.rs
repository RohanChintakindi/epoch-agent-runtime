use std::{fs, str::FromStr, sync::Arc, thread};

use epoch_blob::{BlobError, BlobHash, BlobStore};
use tempfile::TempDir;

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
    assert_eq!(final_files.len(), 1);
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
