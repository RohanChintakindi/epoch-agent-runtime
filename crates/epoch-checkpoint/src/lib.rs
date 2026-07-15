//! Narrow checkpoint backend contracts and observable application-context snapshots.

mod application;

pub use application::{
    APPLICATION_CONTEXT_MEDIA_TYPE, APPLICATION_CONTEXT_SCHEMA_VERSION, ApplicationCheckpoint,
    ApplicationCheckpointBackend, ApplicationCheckpointMetadata, ApplicationContext, MessageRole,
    ObservableMessage, PendingTask, ResumeCursors,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendOutcome<T> {
    Supported(T),
    Unsupported(CheckpointUnsupported),
    Failed(CheckpointFailure),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointUnsupported {
    pub code: UnsupportedCode,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedCode {
    SchemaVersion,
    CooperationRequired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointFailure {
    pub stage: FailureStage,
    pub code: FailureCode,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureStage {
    Capture,
    Load,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureCode {
    InvalidContext,
    MissingReference,
    Storage,
    Integrity,
    Decode,
    NonCanonical,
    MetadataMismatch,
}

/// Backend boundary shared by application checkpoints and future checkpoint components.
pub trait CheckpointBackend {
    type State;
    type Artifact;

    fn capture(&self, state: &Self::State) -> BackendOutcome<Self::Artifact>;
    fn restore(&self, artifact: &Self::Artifact) -> BackendOutcome<Self::State>;
}
