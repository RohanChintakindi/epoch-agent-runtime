//! Direct-process supervision for observable Epoch workloads.

mod fork;
mod manifest;
mod recovery;
mod stream;
mod supervisor;

pub use fork::{
    BoundaryOutcome, EffectFrontierBoundary, ForkBranchReport, RecordedModelResult,
    RecordedReplayResults, RecordedToolResult, ReplayReport, UnsupportedBoundary,
};
pub use manifest::{
    MAX_ARGUMENT_BYTES, MAX_ARGUMENTS, MAX_MANIFEST_BYTES, ManifestError, WorkloadManifest,
};
pub use recovery::{
    ApplicationCheckpointReport, ApplicationEpochDiffReport, ApplicationRestoreMode,
    ApplicationRestoreReport, ApplicationStatusReport, RecoveryCode, RecoveryIssue,
    RecoveryOutcome, RestoreScope,
};
pub use supervisor::{
    AgentTermination, BranchStatus, DirectSupervisor, EventPageReport, EventPageRequest,
    ExecutionError, InspectionError, MAX_STDERR_BYTES, MAX_STDOUT_BYTES, ObservedEvent, RunOutcome,
    SessionStatusReport, SupervisorError,
};
