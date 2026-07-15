#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
ML_PROJECT="${REPO_ROOT}/ml/branch-value"

fail() {
  printf 'ml cross-language smoke failed: %s\n' "$*" >&2
  exit 1
}

for required_command in cargo uv sed grep cksum stat; do
  command -v "${required_command}" >/dev/null 2>&1 \
    || fail "required command is unavailable: ${required_command}"
done

[[ -f "${REPO_ROOT}/Cargo.toml" ]] || fail "repository root is missing Cargo.toml"
[[ -f "${ML_PROJECT}/uv.lock" ]] || fail "ML project is missing its lockfile"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  CARGO_TARGET_DIR="${REPO_ROOT}/target"
elif [[ "${CARGO_TARGET_DIR}" != /* ]]; then
  CARGO_TARGET_DIR="${REPO_ROOT}/${CARGO_TARGET_DIR}"
fi
export CARGO_TARGET_DIR

TEMP_BASE="${TMPDIR:-/tmp}"
TEMP_BASE="${TEMP_BASE%/}"
umask 077
TEMP_ROOT="$(mktemp -d "${TEMP_BASE}/epoch-ml-smoke.XXXXXX")"
chmod 700 "${TEMP_ROOT}"

cleanup() {
  if [[ -n "${TEMP_ROOT:-}" && "${TEMP_ROOT}" == "${TEMP_BASE}"/epoch-ml-smoke.* ]]; then
    rm -rf -- "${TEMP_ROOT}"
  else
    printf 'refusing to clean unexpected smoke path: %s\n' "${TEMP_ROOT:-<unset>}" >&2
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

AGENT="${TEMP_ROOT}/agent.sh"
MANIFEST="${TEMP_ROOT}/workload.toml"
RUN_REPORT="${TEMP_ROOT}/run.json"
RUST_DATASET="${TEMP_ROOT}/rust-trajectories.jsonl"
SYNTHETIC_DATASET="${TEMP_ROOT}/synthetic-trajectories.jsonl"
MODEL_DIR="${TEMP_ROOT}/model"
EVALUATION_REPORT="${TEMP_ROOT}/evaluation.json"
SCORES="${TEMP_ROOT}/scores.jsonl"
RUST_SCORES="${TEMP_ROOT}/rust-scores.jsonl"

apply_private_mode() {
  chmod 600 "$1"
}

write_agent() {
  local destination="$1"
  {
    printf '%s\n' '#!/bin/sh'
    printf '%s\n' 'set -eu'
    printf '%s\n' \
      'printf '"'"'{"payload":{"agent_id":"ml-smoke-agent","branch_id":"%s","session_id":"%s"},"protocol_version":1,"sequence":0,"type":"agent.start"}\n'"'"' "$EPOCH_BRANCH_ID" "$EPOCH_SESSION_ID"'
    printf '%s\n' \
      'printf '"'"'{"payload":{"outcome":"succeeded","output_hash":null},"protocol_version":1,"sequence":1,"type":"agent.completion"}\n'"'"''
  } >"${destination}"
  chmod 700 "${destination}"
}

write_manifest() {
  local destination="$1"
  {
    printf '%s\n' 'schema_version = 1'
    printf '%s\n' 'name = "ml-cross-language-smoke"'
    printf '%s\n' 'executable = "agent.sh"'
  } >"${destination}"
  apply_private_mode "${destination}"
}

file_checksum() {
  cksum <"$1"
}

file_mode() {
  local mode
  if mode="$(stat -f '%Lp' "$1" 2>/dev/null)"; then
    printf '%s\n' "${mode}"
  elif mode="$(stat -c '%a' "$1" 2>/dev/null)"; then
    printf '%s\n' "${mode}"
  else
    fail "could not inspect permissions for $1"
  fi
}

assert_mode() {
  local path="$1"
  local expected="$2"
  local actual
  actual="$(file_mode "${path}")"
  [[ "${actual}" == "${expected}" ]] \
    || fail "unsafe permissions on ${path}: expected ${expected}, got ${actual}"
}

model_fingerprint() {
  local artifact
  for artifact in model.pt model.json split.json training-metrics.json manifest.json; do
    [[ -s "${MODEL_DIR}/${artifact}" ]] || fail "missing model artifact: ${artifact}"
    assert_mode "${MODEL_DIR}/${artifact}" 600
    printf '%s ' "${artifact}"
    file_checksum "${MODEL_DIR}/${artifact}"
  done
}

printf 'Building Epoch from %s\n' "${REPO_ROOT}"
(
  cd "${REPO_ROOT}"
  cargo build --locked -p epoch-cli --bin epoch
)
EPOCH_BIN="${CARGO_TARGET_DIR}/debug/epoch"
[[ -x "${EPOCH_BIN}" ]] || fail "Epoch binary was not produced at ${EPOCH_BIN}"

write_agent "${AGENT}"
write_manifest "${MANIFEST}"

printf 'Running credential-free deterministic agent\n'
(
  cd "${TEMP_ROOT}"
  "${EPOCH_BIN}" run --manifest "${MANIFEST}"
) >"${RUN_REPORT}"
[[ -s "${RUN_REPORT}" ]] || fail "Epoch run did not emit its JSON report"

# Epoch emits one compact JSON object. Avoid adding jq as a smoke-test dependency.
SESSION_ID="$(
  LC_ALL=C sed -n 's/^.*"session_id":"\([^"]*\)".*$/\1/p' "${RUN_REPORT}"
)"
[[ -n "${SESSION_ID}" ]] || fail "could not extract session_id from Epoch run report"
printf '%s\n' "${SESSION_ID}" \
  | grep -Eq '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$' \
  || fail "Epoch returned a noncanonical session_id"

printf 'Exporting Rust trajectory JSONL\n'
"${EPOCH_BIN}" ml export \
  --state-root "${TEMP_ROOT}/.epoch" \
  --session "${SESSION_ID}" \
  --task-group repo-smoke.task-1 \
  --output "${RUST_DATASET}" >"${TEMP_ROOT}/export.json"
[[ -s "${RUST_DATASET}" ]] || fail "Rust exporter did not create JSONL"
assert_mode "${RUST_DATASET}" 600

export_checksum="$(file_checksum "${RUST_DATASET}")"
if "${EPOCH_BIN}" ml export \
  --state-root "${TEMP_ROOT}/.epoch" \
  --session "${SESSION_ID}" \
  --task-group repo-smoke.task-1 \
  --output "${RUST_DATASET}" \
  >"${TEMP_ROOT}/duplicate-export.stdout" \
  2>"${TEMP_ROOT}/duplicate-export.stderr"; then
  fail "Rust exporter overwrote an existing output"
fi
[[ "$(file_checksum "${RUST_DATASET}")" == "${export_checksum}" ]] \
  || fail "failed Rust re-export changed the existing JSONL"

printf 'Validating the exact Rust JSONL with the Python reader\n'
uv run --project "${ML_PROJECT}" --frozen epoch-branch-value \
  validate "${RUST_DATASET}" >"${TEMP_ROOT}/rust-validation.json"

printf 'Generating, training, and evaluating deterministic synthetic data\n'
uv run --project "${ML_PROJECT}" --frozen epoch-branch-value generate \
  --output "${SYNTHETIC_DATASET}" \
  --task-groups 12 \
  --branches-per-group 2 \
  --seed 101 >"${TEMP_ROOT}/generate.json"
[[ -s "${SYNTHETIC_DATASET}" ]] || fail "synthetic generator did not create JSONL"
assert_mode "${SYNTHETIC_DATASET}" 600

uv run --project "${ML_PROJECT}" --frozen epoch-branch-value train \
  "${SYNTHETIC_DATASET}" \
  --output-dir "${MODEL_DIR}" \
  --seed 23 \
  --split-seed 17 \
  --epochs 1 \
  --batch-size 8 \
  --hidden-size 16 >"${TEMP_ROOT}/train.json"
assert_mode "${MODEL_DIR}" 700
model_checksum="$(model_fingerprint)"

if uv run --project "${ML_PROJECT}" --frozen epoch-branch-value train \
  "${SYNTHETIC_DATASET}" \
  --output-dir "${MODEL_DIR}" \
  --seed 23 \
  --split-seed 17 \
  --epochs 1 \
  --batch-size 8 \
  --hidden-size 16 \
  >"${TEMP_ROOT}/duplicate-train.stdout" \
  2>"${TEMP_ROOT}/duplicate-train.stderr"; then
  fail "training overwrote an existing model directory"
fi
[[ "$(model_fingerprint)" == "${model_checksum}" ]] \
  || fail "failed retraining changed existing model artifacts"

uv run --project "${ML_PROJECT}" --frozen epoch-branch-value evaluate \
  "${SYNTHETIC_DATASET}" \
  --model-dir "${MODEL_DIR}" \
  --split test >"${EVALUATION_REPORT}"
[[ -s "${EVALUATION_REPORT}" ]] || fail "evaluation report is empty"

uv run --project "${ML_PROJECT}" --frozen epoch-branch-value score \
  "${SYNTHETIC_DATASET}" \
  --model-dir "${MODEL_DIR}" \
  --output "${SCORES}"
[[ -s "${SCORES}" ]] || fail "score command did not create predictions"
assert_mode "${SCORES}" 600
score_checksum="$(file_checksum "${SCORES}")"

if uv run --project "${ML_PROJECT}" --frozen epoch-branch-value score \
  "${SYNTHETIC_DATASET}" \
  --model-dir "${MODEL_DIR}" \
  --output "${SCORES}" \
  >"${TEMP_ROOT}/duplicate-score.stdout" \
  2>"${TEMP_ROOT}/duplicate-score.stderr"; then
  fail "score command overwrote existing predictions"
fi
[[ "$(file_checksum "${SCORES}")" == "${score_checksum}" ]] \
  || fail "failed rescoring changed existing predictions"

uv run --project "${ML_PROJECT}" --frozen epoch-branch-value score \
  "${RUST_DATASET}" \
  --model-dir "${MODEL_DIR}" \
  --output "${RUST_SCORES}"
[[ -s "${RUST_SCORES}" ]] || fail "exact Rust JSONL did not produce predictions"
assert_mode "${RUST_SCORES}" 600

printf 'ML cross-language smoke passed\n'
