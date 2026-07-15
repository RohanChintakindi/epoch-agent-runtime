# Workspace checkpoint integration contract

`epoch-workspace` is the deterministic full-copy K03 control backend. It is usable on macOS and
Linux without OverlayFS, reflinks, or CRIU. The supervisor stores the returned manifest hash and
length beside the application component in one committed composite epoch; the backend crate itself
does not write epoch rows or change supervisor/CLI lifecycle state.

```rust
let backend = WorkspaceBackend::open(state_dir.join("blobs"), WorkspaceLimits::default())?;
let snapshot = backend.snapshot(workspace_dir)?;
backend.restore(&snapshot, new_workspace_dir)?;
```

At the product boundary, `epoch checkpoint` captures only the workload manifest's resolved
`working_directory` after the session and branch are completed at a cooperative safe point.
`epoch restore EPOCH --workspace-target NEW_DIRECTORY` requires an explicit absent target.
`process_checkpointed` and `process_restored` remain false because no CRIU/COW process backend is
registered.

## Snapshot contract

The backend walks a real directory without following symlinks. It emits a versioned canonical JSON
manifest with a stable path ordering and records:

- the workspace-root mode;
- every regular file's relative path, lower Unix permission bits, executable bit, byte length, and
  raw-content hash;
- every directory's path and mode, including empty directories;
- every symlink's path and original relative target text;
- the explicit `materialize` hardlink policy.

Raw file and manifest bytes enter the hardened `BlobStore` through `put`; snapshot callers cannot
assert trusted blob hashes. Repeated unchanged snapshots therefore return the same manifest hash.
Changing or deleting source files after capture cannot change an existing snapshot.

`.epoch` subtrees are always excluded. A custom blob root nested under the workspace is also
excluded by its canonical path. A workspace equal to, or nested inside, the managed blob root is
rejected.

The caller must stop or otherwise quiesce workspace mutation at the snapshot safe point. Regular
files are opened with `O_NOFOLLOW` on Unix and checked for identity, length, and modification-time
changes around the read, but this full-copy control backend does not claim a transactionally frozen
view of a directory tree under concurrent renames.

## Restore contract

Restore performs these phases in order:

1. Read and hash-verify the manifest blob.
2. Verify its expected length, size bound, schema, canonical byte encoding, sorted unique paths,
   parent relationships, modes, executable bits, symlink safety, and aggregate limits.
3. Read and hash-verify every referenced file blob and compare every declared length.
4. Confirm that the destination does not exist and that its immediate parent is a real directory.
5. Build a private sibling staging directory using create-new files and validated directory parents.
6. Sync files/directories, apply final modes, and rename the staging tree into place.
7. Sync the destination parent.

No target or staging path is created before all manifest and content validation completes. Absolute,
parent-traversing, Windows-drive, backslash-separated, empty, duplicate, reserved `.epoch`, and
overlong manifest paths fail closed. Symlink targets must remain lexically inside the restored
workspace; they are created but never followed by restore.

The target policy is new-directory/no-clobber. After complete input validation, a deterministic
create-new per-target lock serializes Epoch restorers, including concurrent publication of an empty
workspace. The backend checks the target before staging and immediately before publication, and a
same-parent rename gives atomic visibility on supported local filesystems. Rust's portable
directory rename API does not expose `RENAME_NOREPLACE` on every supported host, so a hostile
non-Epoch process racing an empty target directory into the trusted control-plane parent remains
outside this prototype guarantee. The isolation backend should eventually use a platform-specific
no-replace publish primitive. An ungraceful process kill can leave the fail-closed lock file for
operator cleanup; normal returns and injected failures remove and sync it.

Injected failures after staging creation, after the first entry, and immediately before publication
exercise cleanup. Cleanup first restores owner access on staged real directories/files, does not
follow staged symlinks, and then removes the unpublished tree.

## Bounds and unsupported state

The same caller-supplied limits apply during snapshot and restore: entries, total bytes, per-file
bytes, manifest bytes, depth, path bytes, path-component bytes, and symlink-target bytes. Restore
loads verified file blobs before mutation, so `max_total_bytes` also bounds its content memory.

Explicit behavior:

- hardlinks are materialized as independent restored files;
- sparse files are materialized as their logical byte content;
- FIFOs, sockets, devices, and other special files return typed `Unsupported` errors;
- absolute or escaping symlinks are unsupported rather than silently rewritten;
- non-UTF-8 filenames and symlink targets are unsupported rather than lossily normalized;
- empty files and arbitrary binary file content are supported;
- ownership, ACLs, extended attributes, timestamps, filesystem flags, and sparse extents are not
  captured in this control backend.

On Linux filesystems that permit non-UTF-8 names, capture returns `NonUtf8Path`. Typical macOS
filesystems reject creation of such names before Epoch sees them. Epoch performs no Unicode
normalization, so hashes are deterministic for exact accepted UTF-8 path bytes, not promised to be
portable across filesystems that normalize names differently.

Unix/macOS/Linux preserve the lower `0o777` mode bits and symlinks. Non-Unix builds use an explicit
read-only approximation for modes and return `SymlinksOnPlatform` instead of pretending exact
symlink restoration. OverlayFS/reflink COW backends can later implement the same component contract;
they are not part of this library lane.
