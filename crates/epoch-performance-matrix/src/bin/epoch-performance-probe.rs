use std::{fs, hint::black_box, time::Instant};

#[cfg(target_os = "linux")]
use std::{
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    time::Duration,
};

use nix::sys::{
    resource::{UsageWho, getrusage},
    time::TimeValLike as _,
};
use serde::Serialize;

#[derive(Serialize)]
struct Output {
    workload_runtime_ns: u64,
    cpu_user_ns: u64,
    cpu_system_ns: u64,
    peak_rss_bytes: u64,
    compatibility: &'static str,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("epoch-performance-probe: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mode = std::env::args()
        .nth(1)
        .ok_or_else(|| "expected direct or linux mode".to_owned())?;
    if !matches!(mode.as_str(), "direct" | "linux") {
        return Err(format!("unsupported mode {mode:?}"));
    }
    let started = Instant::now();
    let before = getrusage(UsageWho::RUSAGE_SELF).map_err(|error| error.to_string())?;
    let mut allocation = vec![0_u8; 8 * 1024 * 1024];
    for byte in allocation.iter_mut().step_by(4096) {
        *byte = 1;
    }
    let mut accumulator = 0_u64;
    for value in 0_u64..2_000_000 {
        accumulator = accumulator.wrapping_add(value.rotate_left((value % 31) as u32));
    }
    black_box(accumulator);
    black_box(&allocation);
    fs::write("epoch-performance-probe.txt", b"completed\n").map_err(|error| error.to_string())?;

    let compatibility = if mode == "linux" {
        linux_compatibility()?
    } else {
        "completed"
    };
    let after = getrusage(UsageWho::RUSAGE_SELF).map_err(|error| error.to_string())?;
    let cpu_user_ns = timeval_delta_ns(after.user_time(), before.user_time());
    let cpu_system_ns = timeval_delta_ns(after.system_time(), before.system_time());
    #[cfg(target_vendor = "apple")]
    let peak_rss_bytes = u64::try_from(after.max_rss()).unwrap_or(0);
    #[cfg(not(target_vendor = "apple"))]
    let peak_rss_bytes = u64::try_from(after.max_rss())
        .unwrap_or(0)
        .saturating_mul(1024);
    let output = Output {
        workload_runtime_ns: u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        cpu_user_ns,
        cpu_system_ns,
        peak_rss_bytes,
        compatibility,
    };
    println!(
        "{}",
        serde_json::to_string(&output).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn timeval_delta_ns(after: nix::sys::time::TimeVal, before: nix::sys::time::TimeVal) -> u64 {
    u64::try_from(
        after
            .num_microseconds()
            .saturating_sub(before.num_microseconds()),
    )
    .unwrap_or(0)
    .saturating_mul(1_000)
}

#[cfg(target_os = "linux")]
fn linux_compatibility() -> Result<&'static str, String> {
    let base_read_only = fs::write("/etc/epoch-performance-probe", b"escape\n").is_err();
    let network_blocked = TcpStream::connect_timeout(
        &SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 1), 53).into(),
        Duration::from_millis(100),
    )
    .is_err();
    let status = fs::read_to_string("/proc/self/status").map_err(|error| error.to_string())?;
    let cap_eff_zero = field(&status, "CapEff:") == Some("0000000000000000");
    let no_new_privileges = field(&status, "NoNewPrivs:") == Some("1");
    let seccomp_filter = field(&status, "Seccomp:") == Some("2");
    if base_read_only && network_blocked && cap_eff_zero && no_new_privileges && seccomp_filter {
        Ok("sandbox_policy_enforced")
    } else {
        Err(format!(
            "policy mismatch: base_read_only={base_read_only} network_blocked={network_blocked} cap_eff_zero={cap_eff_zero} no_new_privileges={no_new_privileges} seccomp_filter={seccomp_filter}"
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn linux_compatibility() -> Result<&'static str, String> {
    Err("linux compatibility mode requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn field<'a>(status: &'a str, name: &str) -> Option<&'a str> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(name).map(str::trim))
}

#[cfg(test)]
mod tests {
    use super::probe_workspace_filename;

    #[test]
    fn direct_and_linux_samples_never_share_a_workspace_marker_owner() {
        assert_ne!(
            probe_workspace_filename("direct"),
            probe_workspace_filename("linux")
        );
    }
}
