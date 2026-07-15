//! Deterministic workload used exclusively by the CRIU compatibility runner.

use std::{
    fs::{self, OpenOptions},
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

const MIN_MEMORY_BYTES: u64 = 1_048_576;
const MAX_MEMORY_BYTES: u64 = 1_073_741_824;
const MAX_PROCESSES: u32 = 64;

fn main() {
    if let Err(message) = run() {
        eprintln!("epoch-criu-fixture: {message}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let arguments = Arguments::parse()?;
    if let Some(index) = arguments.child_index {
        return child_loop(&arguments.workspace, index);
    }
    if arguments.scenario == "external_tcp" {
        return Err("external_tcp is intentionally unsupported by this fixture".to_owned());
    }

    let allocation_len = usize::try_from(arguments.memory_bytes)
        .map_err(|_| "memory size does not fit this architecture".to_owned())?;
    let mut allocation = vec![0_u8; allocation_len];
    for byte in allocation.iter_mut().step_by(4096) {
        *byte = 1;
    }

    let mut children = if arguments.scenario == "process_tree" {
        spawn_children(&arguments)?
    } else {
        Vec::new()
    };
    let mut open_file = if arguments.scenario == "open_regular_file" {
        Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(arguments.workspace.join("open-file.log"))
                .map_err(|error| format!("open regular file: {error}"))?,
        )
    } else {
        None
    };
    let mut loopback = if arguments.scenario == "loopback_socket" {
        Some(open_loopback_pair()?)
    } else {
        None
    };
    fs::write(
        arguments.workspace.join("ready"),
        arguments.scenario.as_bytes(),
    )
    .map_err(|error| format!("write ready marker: {error}"))?;

    let mut heartbeat = 0_u64;
    loop {
        heartbeat = heartbeat.saturating_add(1);
        fs::write(
            arguments.workspace.join("heartbeat"),
            format!("{heartbeat}\n"),
        )
        .map_err(|error| format!("write heartbeat: {error}"))?;
        if let Some(file) = open_file.as_mut() {
            writeln!(file, "{heartbeat}")
                .and_then(|()| file.flush())
                .map_err(|error| format!("append open file: {error}"))?;
        }
        if let Some((client, server)) = loopback.as_mut() {
            client
                .write_all(&[u8::try_from(heartbeat & 0xff).expect("masked byte")])
                .map_err(|error| format!("loopback write: {error}"))?;
            let mut byte = [0_u8; 1];
            server
                .read_exact(&mut byte)
                .map_err(|error| format!("loopback read: {error}"))?;
        }
        if arguments.scenario == "workspace_mutation" {
            fs::write(
                arguments.workspace.join("workspace-mutation.txt"),
                format!("revision={heartbeat}\n"),
            )
            .map_err(|error| format!("workspace mutation: {error}"))?;
        }
        children.retain_mut(|child| child.try_wait().is_ok_and(|status| status.is_none()));
        std::hint::black_box(&allocation);
        thread::sleep(Duration::from_millis(50));
    }
}

fn child_loop(workspace: &Path, index: u32) -> Result<(), String> {
    let path = workspace.join(format!("child-{index}-heartbeat"));
    let mut heartbeat = 0_u64;
    loop {
        heartbeat = heartbeat.saturating_add(1);
        fs::write(&path, format!("{heartbeat}\n"))
            .map_err(|error| format!("write child heartbeat: {error}"))?;
        thread::sleep(Duration::from_millis(50));
    }
}

fn spawn_children(arguments: &Arguments) -> Result<Vec<Child>, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("resolve current executable: {error}"))?;
    let workspace = arguments
        .workspace
        .to_str()
        .ok_or_else(|| "workspace path is not UTF-8".to_owned())?;
    (1..arguments.process_count)
        .map(|index| {
            Command::new(&executable)
                .args(["--child", &index.to_string(), "--workspace", workspace])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| format!("spawn child {index}: {error}"))
        })
        .collect()
}

fn open_loopback_pair() -> Result<(TcpStream, TcpStream), String> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .map_err(|error| format!("bind loopback: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("read loopback address: {error}"))?;
    let client =
        TcpStream::connect(address).map_err(|error| format!("connect loopback: {error}"))?;
    let (server, _) = listener
        .accept()
        .map_err(|error| format!("accept loopback: {error}"))?;
    Ok((client, server))
}

struct Arguments {
    scenario: String,
    workspace: PathBuf,
    memory_bytes: u64,
    process_count: u32,
    child_index: Option<u32>,
}

impl Arguments {
    fn parse() -> Result<Self, String> {
        let mut scenario = None;
        let mut workspace = None;
        let mut memory_bytes = MIN_MEMORY_BYTES;
        let mut process_count = 1;
        let mut child_index = None;
        let mut values = std::env::args().skip(1);
        while let Some(argument) = values.next() {
            match argument.as_str() {
                "--scenario" => scenario = values.next(),
                "--workspace" => workspace = values.next().map(PathBuf::from),
                "--memory-bytes" => {
                    memory_bytes = parse_value(values.next(), "memory-bytes")?;
                }
                "--process-count" => {
                    process_count = parse_value(values.next(), "process-count")?;
                }
                "--child" => child_index = Some(parse_value(values.next(), "child")?),
                _ => return Err(format!("unknown argument {argument:?}")),
            }
        }
        let workspace = workspace.ok_or_else(|| "missing --workspace".to_owned())?;
        if !workspace.is_absolute() || !workspace.is_dir() {
            return Err("workspace must be an existing absolute directory".to_owned());
        }
        if !(MIN_MEMORY_BYTES..=MAX_MEMORY_BYTES).contains(&memory_bytes) {
            return Err("memory-bytes is outside fixture bounds".to_owned());
        }
        if !(1..=MAX_PROCESSES).contains(&process_count) {
            return Err("process-count is outside fixture bounds".to_owned());
        }
        Ok(Self {
            scenario: scenario.unwrap_or_else(|| "child".to_owned()),
            workspace,
            memory_bytes,
            process_count,
            child_index,
        })
    }
}

fn parse_value<T: std::str::FromStr>(value: Option<String>, field: &str) -> Result<T, String> {
    value
        .ok_or_else(|| format!("missing --{field} value"))?
        .parse()
        .map_err(|_| format!("invalid --{field} value"))
}
