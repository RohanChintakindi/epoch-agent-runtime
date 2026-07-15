//! Native Linux isolation probe used only by the privileged integration suite.

use std::{
    fs,
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    process::{Command, Stdio},
    time::Duration,
};

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let result = match mode.as_str() {
        "inspect" => inspect(),
        "fork-limit" => {
            fork_limit();
            Ok(())
        }
        "memory-limit" => memory_limit(),
        _ => Err(format!("unknown probe mode {mode:?}")),
    };
    if let Err(message) = result {
        eprintln!("epoch-sandbox-probe: {message}");
        std::process::exit(2);
    }
}

fn inspect() -> Result<(), String> {
    fs::write("workspace-write.txt", b"sandboxed\n").map_err(|error| error.to_string())?;
    let base_read_only = fs::write("/etc/epoch-sandbox-escape", b"escape\n").is_err();
    fs::write("/tmp/epoch-private-marker", b"private\n").map_err(|error| error.to_string())?;
    let network_blocked = TcpStream::connect_timeout(
        &SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 1), 53).into(),
        Duration::from_millis(100),
    )
    .is_err();
    let child_spawned = Command::new("/usr/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    let unshare_blocked = Command::new("/usr/bin/unshare")
        .args(["--user", "/usr/bin/true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| !status.success());
    let status = fs::read_to_string("/proc/self/status").map_err(|error| error.to_string())?;
    let numeric_processes = fs::read_dir("/proc")
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|byte| byte.is_ascii_digit())
        })
        .count();

    println!("pid={}", std::process::id());
    println!("numeric_processes={numeric_processes}");
    println!("base_read_only={base_read_only}");
    println!("network_blocked={network_blocked}");
    println!("child_spawned={child_spawned}");
    println!("unshare_blocked={unshare_blocked}");
    for field in [
        "Uid:",
        "CapPrm:",
        "CapEff:",
        "CapBnd:",
        "CapAmb:",
        "NoNewPrivs:",
        "Seccomp:",
    ] {
        if let Some(line) = status.lines().find(|line| line.starts_with(field)) {
            println!(
                "status_{}={}",
                field.trim_end_matches(':'),
                line[field.len()..].trim()
            );
        }
    }
    Ok(())
}

fn fork_limit() {
    let mut children = Vec::new();
    for _ in 0..64 {
        match Command::new("/usr/bin/sleep")
            .arg("5")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => children.push(child),
            Err(_) => break,
        }
    }
    println!("spawned={}", children.len());
    for mut child in children {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn memory_limit() -> Result<(), String> {
    let mut allocations = Vec::new();
    loop {
        let mut chunk = vec![0_u8; 4 * 1024 * 1024];
        for byte in chunk.iter_mut().step_by(4096) {
            *byte = 1;
        }
        allocations.push(chunk);
        println!("allocated_chunks={}", allocations.len());
    }
}
