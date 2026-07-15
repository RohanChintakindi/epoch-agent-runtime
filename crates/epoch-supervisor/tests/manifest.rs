#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

use epoch_supervisor::{MAX_ARGUMENTS, MAX_MANIFEST_BYTES, ManifestError, WorkloadManifest};
use tempfile::TempDir;

fn executable(path: &Path) {
    fs::write(path, "#!/bin/sh\nexit 0\n").expect("write fixture executable");
    let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).expect("mark fixture executable");
}

fn write_manifest(directory: &Path, contents: &str) -> PathBuf {
    let path = directory.join("workload.toml");
    fs::write(&path, contents).expect("write fixture manifest");
    path
}

#[test]
fn loads_a_minimal_manifest_and_resolves_paths_relative_to_it() {
    let directory = TempDir::new().expect("create fixture directory");
    fs::create_dir(directory.path().join("workspace")).expect("create working directory");
    executable(&directory.path().join("agent"));
    let manifest_path = write_manifest(
        directory.path(),
        r#"
schema_version = 1
name = "deterministic-agent"
executable = "./agent"
arguments = ["--seed", "99", "literal argument with spaces"]
working_directory = "./workspace"
"#,
    );

    let manifest = WorkloadManifest::load(manifest_path).expect("load valid manifest");
    assert_eq!(manifest.name, "deterministic-agent");
    assert_eq!(
        manifest.executable,
        fs::canonicalize(directory.path().join("agent")).expect("canonical fixture executable")
    );
    assert_eq!(
        manifest.arguments,
        ["--seed", "99", "literal argument with spaces"]
    );
    assert_eq!(
        manifest.working_directory,
        fs::canonicalize(directory.path().join("workspace")).expect("canonical fixture workspace")
    );
}

#[test]
fn defaults_working_directory_to_manifest_directory() {
    let directory = TempDir::new().expect("create fixture directory");
    executable(&directory.path().join("agent"));
    let path = write_manifest(
        directory.path(),
        "schema_version = 1\nname = \"agent\"\nexecutable = \"agent\"\n",
    );
    let manifest = WorkloadManifest::load(path).expect("load valid manifest");
    assert_eq!(
        manifest.working_directory,
        fs::canonicalize(directory.path()).expect("canonical fixture directory")
    );
    assert!(manifest.arguments.is_empty());
}

#[test]
fn rejects_unknown_environment_and_secret_fields() {
    let directory = TempDir::new().expect("create fixture directory");
    executable(&directory.path().join("agent"));

    for forbidden in [
        "environment = { API_KEY = \"secret\" }",
        "secrets = { token = \"secret\" }",
        "unexpected = true",
    ] {
        let path = write_manifest(
            directory.path(),
            &format!("schema_version = 1\nname = \"agent\"\nexecutable = \"agent\"\n{forbidden}\n"),
        );
        assert!(matches!(
            WorkloadManifest::load(path),
            Err(ManifestError::Parse { .. })
        ));
    }
}

#[test]
fn rejects_oversized_manifest_before_parsing() {
    let directory = TempDir::new().expect("create fixture directory");
    let path = directory.path().join("workload.toml");
    fs::write(&path, vec![b'x'; MAX_MANIFEST_BYTES + 1]).expect("write oversized fixture");
    assert!(matches!(
        WorkloadManifest::load(path),
        Err(ManifestError::TooLarge {
            maximum: MAX_MANIFEST_BYTES
        })
    ));
}

#[test]
fn rejects_unsupported_versions_and_unsafe_names() {
    let directory = TempDir::new().expect("create fixture directory");
    executable(&directory.path().join("agent"));
    for (version, name) in [(2, "agent"), (1, "agent\nforged-log")] {
        let path = write_manifest(
            directory.path(),
            &format!("schema_version = {version}\nname = {name:?}\nexecutable = \"agent\"\n"),
        );
        let error = WorkloadManifest::load(path).expect_err("manifest must be rejected");
        if version == 2 {
            assert!(matches!(
                error,
                ManifestError::UnsupportedVersion { received: 2 }
            ));
        } else {
            assert!(matches!(error, ManifestError::InvalidName));
        }
    }
}

#[test]
fn rejects_nonexecutables_missing_directories_and_argument_abuse() {
    let directory = TempDir::new().expect("create fixture directory");
    fs::write(directory.path().join("not-executable"), "data").expect("write fixture file");

    let cases = [
        (
            "schema_version = 1\nname = \"agent\"\nexecutable = \"not-executable\"\n",
            "executable",
        ),
        (
            "schema_version = 1\nname = \"agent\"\nexecutable = \"not-executable\"\nworking_directory = \"missing\"\n",
            "path",
        ),
    ];
    for (contents, _label) in cases {
        let path = write_manifest(directory.path(), contents);
        assert!(WorkloadManifest::load(path).is_err());
    }

    executable(&directory.path().join("agent"));
    let arguments = std::iter::repeat_n("\"x\"", MAX_ARGUMENTS + 1)
        .collect::<Vec<_>>()
        .join(",");
    let path = write_manifest(
        directory.path(),
        &format!(
            "schema_version = 1\nname = \"agent\"\nexecutable = \"agent\"\narguments = [{arguments}]\n"
        ),
    );
    assert!(matches!(
        WorkloadManifest::load(path),
        Err(ManifestError::TooManyArguments { .. })
    ));
}
