use std::process::Command;

#[cfg(target_os = "linux")]
use std::fs;

use crate::{BenchmarkEnvironment, HostMemory, MatrixError, validate_revision};

/// Discovers immutable host facts and applies a caller-declared memory ceiling.
///
/// # Errors
///
/// Returns an error when the source revision is not an exact Git object ID.
pub fn discover_environment(
    code_revision: &str,
    maximum_safety_budget_bytes: u64,
) -> Result<BenchmarkEnvironment, MatrixError> {
    validate_revision(code_revision)?;
    let available_bytes = available_memory_bytes();
    let cgroup_bytes = cgroup_available_bytes().unwrap_or(u64::MAX);
    let discovered = available_bytes.min(cgroup_bytes);
    let half_available = discovered / 2;
    let safety_budget_bytes = half_available.min(maximum_safety_budget_bytes);
    Ok(BenchmarkEnvironment {
        operating_system: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        kernel_release: kernel_release(),
        logical_cpus: std::thread::available_parallelism().map_or(1, usize::from),
        code_revision: code_revision.to_owned(),
        host_memory: HostMemory {
            available_bytes,
            safety_budget_bytes,
        },
    })
}

fn kernel_release() -> String {
    Command::new("/usr/bin/uname")
        .arg("-r")
        .env_clear()
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn available_memory_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(contents) = fs::read_to_string("/proc/meminfo")
            && let Some(kib) = contents.lines().find_map(|line| {
                line.strip_prefix("MemAvailable:")
                    .and_then(|value| value.split_ascii_whitespace().next())
                    .and_then(|value| value.parse::<u64>().ok())
            })
        {
            return kib.saturating_mul(1024);
        }
    }
    0
}

fn cgroup_available_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let maximum = fs::read_to_string("/sys/fs/cgroup/memory.max").ok()?;
        let maximum = maximum.trim().parse::<u64>().ok()?;
        let current = fs::read_to_string("/sys/fs/cgroup/memory.current")
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()?;
        return Some(maximum.saturating_sub(current));
    }
    #[cfg(not(target_os = "linux"))]
    None
}
