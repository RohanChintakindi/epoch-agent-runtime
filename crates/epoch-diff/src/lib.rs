//! Stable semantic differences between validated Epoch checkpoint components.
//!
//! The public entry point accepts checkpoint records rather than decoded contexts. Both sides are
//! restored through [`ApplicationCheckpointBackend`] before comparison, so corrupt, non-canonical,
//! metadata-mismatched, and otherwise unvalidated input never reaches the pure diff engine.

use std::{collections::BTreeMap, fmt};

use epoch_blob::BlobHash;
use epoch_checkpoint::{
    ApplicationCheckpoint, ApplicationCheckpointBackend, ApplicationContext, BackendOutcome,
    CheckpointBackend, FailureCode, ObservableMessage, PendingTask, UnsupportedCode,
};
use serde::Serialize;
use serde_json::{Value, json};

/// Schema version for the machine-readable semantic diff document.
pub const SEMANTIC_DIFF_SCHEMA_VERSION: u16 = 1;

const APPLICATION_CONTEXT_V1_UNSUPPORTED_REASON: &str =
    "not represented by application context schema 1";

/// Application-context sections that cannot be compared until a composite manifest supplies them.
pub const UNSUPPORTED_APPLICATION_SECTIONS: [UnsupportedSection; 3] = [
    UnsupportedSection {
        section: SemanticSection::Capabilities,
        reason: APPLICATION_CONTEXT_V1_UNSUPPORTED_REASON,
    },
    UnsupportedSection {
        section: SemanticSection::EffectFrontier,
        reason: APPLICATION_CONTEXT_V1_UNSUPPORTED_REASON,
    },
    UnsupportedSection {
        section: SemanticSection::WorkspaceFiles,
        reason: APPLICATION_CONTEXT_V1_UNSUPPORTED_REASON,
    },
];

/// One stable, versioned comparison of two application checkpoint components.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApplicationSemanticDiff {
    pub schema_version: u16,
    pub before_component_hash: BlobHash,
    pub after_component_hash: BlobHash,
    pub identical: bool,
    pub unsupported_sections: Vec<UnsupportedSection>,
    pub changes: Vec<SemanticChange>,
}

impl ApplicationSemanticDiff {
    /// Encodes the diff as compact deterministic JSON.
    ///
    /// Fields and changes are constructed in stable order, and every map-derived change is sorted
    /// before this method is called.
    ///
    /// # Errors
    ///
    /// Returns a JSON serialization error if the document cannot be encoded.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

/// A state section understood by semantic diff schema 1.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticSection {
    Application,
    Cursors,
    Messages,
    MemoryReferences,
    PendingModelRequests,
    PendingTasks,
    PendingToolCalls,
    ToolRegistry,
    Capabilities,
    EffectFrontier,
    WorkspaceFiles,
}

/// An explicit statement that a semantic section is not present in this input schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct UnsupportedSection {
    pub section: SemanticSection,
    pub reason: &'static str,
}

/// Whether a semantic entity or field appeared, disappeared, or changed value.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeClassification {
    Added,
    Removed,
    Changed,
}

/// One stable semantic change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SemanticChange {
    pub section: SemanticSection,
    pub path: String,
    pub classification: ChangeClassification,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
}

/// Which checkpoint failed trusted loading.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    Before,
    After,
}

/// Stable high-level classification for a refused comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffErrorKind {
    UnsupportedSchema,
    UnsupportedCheckpoint,
    InvalidCheckpoint,
}

/// A structured refusal to compare input that was not successfully validated.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiffError {
    pub side: DiffSide,
    pub kind: DiffErrorKind,
    pub code: &'static str,
    pub detail: String,
}

impl fmt::Display for DiffError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} checkpoint cannot be diffed ({}): {}",
            self.side, self.code, self.detail
        )
    }
}

impl std::error::Error for DiffError {}

/// Compares two application checkpoint components after trusted restore and validation.
///
/// The application backend validates content hashes, canonical encoding, schema, nested blob
/// references, context shape, and checkpoint metadata. Future composite-manifest integration can
/// invoke analogous section-specific engines after validating each component.
///
/// # Errors
///
/// Returns [`DiffError`] when either side is unsupported or fails trusted checkpoint restoration.
pub fn diff_application_checkpoints(
    backend: &ApplicationCheckpointBackend,
    before: &ApplicationCheckpoint,
    after: &ApplicationCheckpoint,
) -> Result<ApplicationSemanticDiff, DiffError> {
    let before_context = validated_context(backend, before, DiffSide::Before)?;
    let after_context = validated_context(backend, after, DiffSide::After)?;
    let mut changes = compare_contexts(&before_context, &after_context);
    changes.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.classification.cmp(&right.classification))
            .then(left.section.cmp(&right.section))
    });

    Ok(ApplicationSemanticDiff {
        schema_version: SEMANTIC_DIFF_SCHEMA_VERSION,
        before_component_hash: before.component_hash.clone(),
        after_component_hash: after.component_hash.clone(),
        identical: changes.is_empty(),
        unsupported_sections: UNSUPPORTED_APPLICATION_SECTIONS.to_vec(),
        changes,
    })
}

fn validated_context(
    backend: &ApplicationCheckpointBackend,
    checkpoint: &ApplicationCheckpoint,
    side: DiffSide,
) -> Result<ApplicationContext, DiffError> {
    match backend.restore(checkpoint) {
        BackendOutcome::Supported(context) => Ok(context),
        BackendOutcome::Unsupported(unsupported) => {
            let (kind, code) = match unsupported.code {
                UnsupportedCode::SchemaVersion => {
                    (DiffErrorKind::UnsupportedSchema, "schema_version")
                }
                UnsupportedCode::CooperationRequired => {
                    (DiffErrorKind::UnsupportedCheckpoint, "cooperation_required")
                }
            };
            Err(DiffError {
                side,
                kind,
                code,
                detail: unsupported.detail,
            })
        }
        BackendOutcome::Failed(failure) => Err(DiffError {
            side,
            kind: DiffErrorKind::InvalidCheckpoint,
            code: failure_code(failure.code),
            detail: failure.detail,
        }),
    }
}

const fn failure_code(code: FailureCode) -> &'static str {
    match code {
        FailureCode::InvalidContext => "invalid_context",
        FailureCode::MissingReference => "missing_reference",
        FailureCode::Storage => "storage",
        FailureCode::Integrity => "integrity",
        FailureCode::Decode => "decode",
        FailureCode::NonCanonical => "non_canonical",
        FailureCode::MetadataMismatch => "metadata_mismatch",
    }
}

fn compare_contexts(
    before: &ApplicationContext,
    after: &ApplicationContext,
) -> Vec<SemanticChange> {
    let mut changes = Vec::new();

    changed_value(
        &mut changes,
        SemanticSection::Application,
        "/safe_point_id",
        &before.safe_point_id,
        &after.safe_point_id,
    );
    changed_value(
        &mut changes,
        SemanticSection::Application,
        "/deterministic_seed",
        &before.deterministic_seed,
        &after.deterministic_seed,
    );
    changed_value(
        &mut changes,
        SemanticSection::Application,
        "/context_revision",
        &before.context_revision,
        &after.context_revision,
    );
    changed_value(
        &mut changes,
        SemanticSection::Application,
        "/model_identifier",
        &before.model_identifier,
        &after.model_identifier,
    );
    changed_value(
        &mut changes,
        SemanticSection::Cursors,
        "/cursors/boundary_sequence",
        &before.cursors.boundary_sequence,
        &after.cursors.boundary_sequence,
    );
    changed_value(
        &mut changes,
        SemanticSection::Cursors,
        "/cursors/message_cursor",
        &before.cursors.message_cursor,
        &after.cursors.message_cursor,
    );
    changed_value(
        &mut changes,
        SemanticSection::Cursors,
        "/cursors/tool_cursor",
        &before.cursors.tool_cursor,
        &after.cursors.tool_cursor,
    );
    changed_value(
        &mut changes,
        SemanticSection::Cursors,
        "/cursors/task_cursor",
        &before.cursors.task_cursor,
        &after.cursors.task_cursor,
    );

    compare_string_map(
        &mut changes,
        SemanticSection::ToolRegistry,
        "/tool_registry",
        &before.tool_registry,
        &after.tool_registry,
    );
    compare_messages(&mut changes, &before.messages, &after.messages);
    compare_tasks(&mut changes, &before.pending_tasks, &after.pending_tasks);
    compare_id_collection(
        &mut changes,
        SemanticSection::PendingModelRequests,
        "/pending_model_request_ids",
        &before.pending_model_request_ids,
        &after.pending_model_request_ids,
    );
    compare_id_collection(
        &mut changes,
        SemanticSection::PendingToolCalls,
        "/pending_tool_call_ids",
        &before.pending_tool_call_ids,
        &after.pending_tool_call_ids,
    );
    changed_value(
        &mut changes,
        SemanticSection::MemoryReferences,
        "/user_visible_summary_hash",
        &before.user_visible_summary_hash,
        &after.user_visible_summary_hash,
    );

    changes
}

fn changed_value<T>(
    changes: &mut Vec<SemanticChange>,
    section: SemanticSection,
    path: &str,
    before: &T,
    after: &T,
) where
    T: Eq + Serialize,
{
    if before != after {
        changes.push(SemanticChange {
            section,
            path: path.to_owned(),
            classification: ChangeClassification::Changed,
            before: Some(json!(before)),
            after: Some(json!(after)),
        });
    }
}

fn compare_string_map(
    changes: &mut Vec<SemanticChange>,
    section: SemanticSection,
    root: &str,
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) {
    for (identifier, before_value) in before {
        let path = entity_path(root, identifier);
        match after.get(identifier) {
            Some(after_value) => {
                changed_value(changes, section, &path, before_value, after_value);
            }
            None => removed(changes, section, path, json!(before_value)),
        }
    }
    for (identifier, after_value) in after {
        if !before.contains_key(identifier) {
            added(
                changes,
                section,
                entity_path(root, identifier),
                json!(after_value),
            );
        }
    }
}

fn compare_messages(
    changes: &mut Vec<SemanticChange>,
    before: &[ObservableMessage],
    after: &[ObservableMessage],
) {
    let before_by_id = before
        .iter()
        .map(|message| (message.message_id.as_str(), message))
        .collect::<BTreeMap<_, _>>();
    let after_by_id = after
        .iter()
        .map(|message| (message.message_id.as_str(), message))
        .collect::<BTreeMap<_, _>>();

    compare_stable_id_order(
        changes,
        SemanticSection::Messages,
        "/messages/order",
        before.iter().map(|message| message.message_id.as_str()),
        after.iter().map(|message| message.message_id.as_str()),
    );

    for (identifier, before_message) in &before_by_id {
        let root = entity_path("/messages", identifier);
        match after_by_id.get(identifier) {
            Some(after_message) => {
                changed_value(
                    changes,
                    SemanticSection::Messages,
                    &format!("{root}/role"),
                    &before_message.role,
                    &after_message.role,
                );
                changed_value(
                    changes,
                    SemanticSection::Messages,
                    &format!("{root}/content_hash"),
                    &before_message.content_hash,
                    &after_message.content_hash,
                );
            }
            None => removed(
                changes,
                SemanticSection::Messages,
                root,
                json!(before_message),
            ),
        }
    }
    for (identifier, after_message) in &after_by_id {
        if !before_by_id.contains_key(identifier) {
            added(
                changes,
                SemanticSection::Messages,
                entity_path("/messages", identifier),
                json!(after_message),
            );
        }
    }
}

fn compare_tasks(changes: &mut Vec<SemanticChange>, before: &[PendingTask], after: &[PendingTask]) {
    let before_by_id = before
        .iter()
        .map(|task| (task.task_id.as_str(), task))
        .collect::<BTreeMap<_, _>>();
    let after_by_id = after
        .iter()
        .map(|task| (task.task_id.as_str(), task))
        .collect::<BTreeMap<_, _>>();

    compare_stable_id_order(
        changes,
        SemanticSection::PendingTasks,
        "/pending_tasks/order",
        before.iter().map(|task| task.task_id.as_str()),
        after.iter().map(|task| task.task_id.as_str()),
    );

    for (identifier, before_task) in &before_by_id {
        let root = entity_path("/pending_tasks", identifier);
        match after_by_id.get(identifier) {
            Some(after_task) => {
                changed_value(
                    changes,
                    SemanticSection::PendingTasks,
                    &format!("{root}/task_type"),
                    &before_task.task_type,
                    &after_task.task_type,
                );
                changed_value(
                    changes,
                    SemanticSection::PendingTasks,
                    &format!("{root}/payload_hash"),
                    &before_task.payload_hash,
                    &after_task.payload_hash,
                );
            }
            None => removed(
                changes,
                SemanticSection::PendingTasks,
                root,
                json!(before_task),
            ),
        }
    }
    for (identifier, after_task) in &after_by_id {
        if !before_by_id.contains_key(identifier) {
            added(
                changes,
                SemanticSection::PendingTasks,
                entity_path("/pending_tasks", identifier),
                json!(after_task),
            );
        }
    }
}

fn compare_id_collection(
    changes: &mut Vec<SemanticChange>,
    section: SemanticSection,
    root: &str,
    before: &[String],
    after: &[String],
) {
    let before_by_id = before
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let after_by_id = after
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    for identifier in before_by_id.difference(&after_by_id) {
        removed(
            changes,
            section,
            entity_path(root, identifier),
            json!(identifier),
        );
    }
    for identifier in after_by_id.difference(&before_by_id) {
        added(
            changes,
            section,
            entity_path(root, identifier),
            json!(identifier),
        );
    }
}

fn compare_stable_id_order<'a>(
    changes: &mut Vec<SemanticChange>,
    section: SemanticSection,
    path: &str,
    before: impl Iterator<Item = &'a str>,
    after: impl Iterator<Item = &'a str>,
) {
    let before_order = before.collect::<Vec<_>>();
    let after_order = after.collect::<Vec<_>>();
    if before_order == after_order {
        return;
    }
    let before_ids = before_order
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let after_ids = after_order
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    if before_ids == after_ids {
        changed_value(changes, section, path, &before_order, &after_order);
    }
}

fn added(changes: &mut Vec<SemanticChange>, section: SemanticSection, path: String, after: Value) {
    changes.push(SemanticChange {
        section,
        path,
        classification: ChangeClassification::Added,
        before: None,
        after: Some(after),
    });
}

fn removed(
    changes: &mut Vec<SemanticChange>,
    section: SemanticSection,
    path: String,
    before: Value,
) {
    changes.push(SemanticChange {
        section,
        path,
        classification: ChangeClassification::Removed,
        before: Some(before),
        after: None,
    });
}

fn entity_path(root: &str, identifier: &str) -> String {
    format!("{root}/{}", json_pointer_token(identifier))
}

fn json_pointer_token(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}
