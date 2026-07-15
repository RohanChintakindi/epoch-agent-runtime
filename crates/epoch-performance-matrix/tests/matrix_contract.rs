use std::collections::BTreeSet;

use epoch_performance_matrix::{
    CowMatrixConfig, HostMemory, PlannedOutcome, REQUIRED_ALLOCATIONS_BYTES,
    REQUIRED_DIRTY_BASIS_POINTS, REQUIRED_FANOUTS, plan_cow_matrix,
};

#[test]
fn required_cow_matrix_is_complete_unique_and_stably_ordered() {
    let config = CowMatrixConfig::required();
    let rows = plan_cow_matrix(
        &config,
        HostMemory {
            available_bytes: u64::MAX,
            safety_budget_bytes: u64::MAX,
        },
        true,
    );

    assert_eq!(rows.len(), 3 * 4 * 5);
    let keys = rows.iter().map(|row| row.key).collect::<BTreeSet<_>>();
    assert_eq!(keys.len(), rows.len());
    assert_eq!(
        config.allocations_bytes.as_slice(),
        REQUIRED_ALLOCATIONS_BYTES
    );
    assert_eq!(config.fanouts.as_slice(), REQUIRED_FANOUTS);
    assert_eq!(
        config.dirty_basis_points.as_slice(),
        REQUIRED_DIRTY_BASIS_POINTS
    );
    assert!(
        rows.windows(2).all(|pair| pair[0].key < pair[1].key),
        "report order must not depend on hash or process ordering"
    );
}

#[test]
fn preflight_keeps_every_declared_row_but_skips_unsafe_processes() {
    let rows = plan_cow_matrix(
        &CowMatrixConfig::required(),
        HostMemory {
            available_bytes: 512 * 1024 * 1024,
            safety_budget_bytes: 128 * 1024 * 1024,
        },
        true,
    );
    assert_eq!(rows.len(), 60);
    assert!(rows.iter().all(|row| matches!(
        row.outcome,
        PlannedOutcome::Skipped { ref code, .. } if code == "memory_preflight"
    )));
}

#[test]
fn non_linux_host_retains_structured_unsupported_rows() {
    let rows = plan_cow_matrix(
        &CowMatrixConfig::required(),
        HostMemory {
            available_bytes: u64::MAX,
            safety_budget_bytes: u64::MAX,
        },
        false,
    );
    assert_eq!(rows.len(), 60);
    assert!(rows.iter().all(|row| matches!(
        row.outcome,
        PlannedOutcome::Unsupported { ref code, .. } if code == "platform_not_linux"
    )));
}
