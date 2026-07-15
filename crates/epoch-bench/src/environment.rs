use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::{Command, Output},
};

use thiserror::Error;

use crate::BenchmarkEnvironment;

const MAX_COMMAND_OUTPUT: usize = 4_096;

/// Failure to collect or validate authoritative benchmark environment facts.
#[derive(Debug, Error)]
pub enum EnvironmentError {
    /// Repository root does not name a real directory.
    #[error("benchmark repository root is not a directory")]
    InvalidRepository,
    /// A required metadata command could not be executed successfully.
    #[error("environment command {command} failed: {detail}")]
    Command {
        /// Stable command label.
        command: &'static str,
        /// Bounded diagnostic.
        detail: String,
    },
    /// Collected metadata failed structural validation.
    #[error("invalid benchmark environment field {field}: {detail}")]
    InvalidField {
        /// Field name.
        field: &'static str,
        /// Validation detail.
        detail: &'static str,
    },
}

impl BenchmarkEnvironment {
    /// Collects source, operating-system, CPU, memory, and runtime facts from the current host.
    ///
    /// # Errors
    ///
    /// Returns a typed failure when the repository is invalid, a required command fails, or the
    /// collected result does not pass structural validation.
    pub fn collect(repository: &Path) -> Result<Self, EnvironmentError> {
        if !repository.is_dir() {
            return Err(EnvironmentError::InvalidRepository);
        }
        let code_revision = command_text(
            Command::new("git")
                .current_dir(repository)
                .args(["rev-parse", "HEAD"]),
            "git_revision",
        )?;
        let status = command_output(
            Command::new("git")
                .current_dir(repository)
                .args(["status", "--porcelain=v1"]),
            "git_status",
        )?;
        let kernel_release = command_text(Command::new("uname").arg("-r"), "kernel_release")?;
        let runtime_version = command_text(Command::new("rustc").arg("--version"), "rustc")?;
        let cpu_count = std::thread::available_parallelism()
            .ok()
            .and_then(|count| u32::try_from(count.get()).ok())
            .unwrap_or(0);
        let environment = Self {
            code_revision,
            code_dirty: !status.stdout.is_empty(),
            os: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            kernel_release,
            cpu_model: collect_cpu_model()?,
            cpu_count,
            total_memory_bytes: collect_memory_bytes(),
            runtime_version,
            extra: BTreeMap::from([
                ("collector".to_owned(), "epoch-bench-v1".to_owned()),
                (
                    "repository".to_owned(),
                    fs::canonicalize(repository)
                        .map_err(|error| EnvironmentError::Command {
                            command: "canonicalize_repository",
                            detail: bounded(&error.to_string()),
                        })?
                        .display()
                        .to_string(),
                ),
            ]),
        };
        environment.validate()?;
        Ok(environment)
    }

    /// Validates that required environment metadata is nonempty and structurally plausible.
    ///
    /// # Errors
    ///
    /// Returns the first invalid required field.
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if !(7..=64).contains(&self.code_revision.len())
            || !self
                .code_revision
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid("code_revision", "expected 7-64 hexadecimal bytes"));
        }
        for (field, value) in [
            ("os", self.os.as_str()),
            ("architecture", self.architecture.as_str()),
            ("kernel_release", self.kernel_release.as_str()),
            ("cpu_model", self.cpu_model.as_str()),
            ("runtime_version", self.runtime_version.as_str()),
        ] {
            if value.trim().is_empty() || value.len() > MAX_COMMAND_OUTPUT {
                return Err(invalid(field, "expected bounded nonempty text"));
            }
        }
        if self.cpu_count == 0 {
            return Err(invalid("cpu_count", "expected at least one logical CPU"));
        }
        if self.total_memory_bytes == Some(0) {
            return Err(invalid(
                "total_memory_bytes",
                "zero is not a valid host size",
            ));
        }
        Ok(())
    }
}

fn collect_cpu_model() -> Result<String, EnvironmentError> {
    #[cfg(target_os = "linux")]
    {
        let mut lscpu_command = Command::new("lscpu");
        if let Ok(lscpu) = command_text(&mut lscpu_command, "cpu_model")
            && let Some(model) = parse_cpu_model(&lscpu)
        {
            return Ok(model);
        }
        let cpuinfo =
            fs::read_to_string("/proc/cpuinfo").map_err(|error| EnvironmentError::Command {
                command: "cpu_model",
                detail: bounded(&error.to_string()),
            })?;
        parse_cpu_model(&cpuinfo).ok_or_else(|| EnvironmentError::Command {
            command: "cpu_model",
            detail: "Linux CPU metadata omitted model and implementer/part identifiers".to_owned(),
        })
    }
    #[cfg(target_os = "macos")]
    {
        command_text(
            Command::new("sysctl").args(["-n", "machdep.cpu.brand_string"]),
            "cpu_model",
        )
        .or_else(|_| command_text(Command::new("sysctl").args(["-n", "hw.model"]), "cpu_model"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Ok(format!("{}-cpu", std::env::consts::ARCH))
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_cpu_model(contents: &str) -> Option<String> {
    for key in ["Model name", "model name", "Hardware", "Processor"] {
        if let Some(value) = field(contents, key) {
            return Some(bounded(value));
        }
    }
    let implementer = field(contents, "CPU implementer")?;
    let part = field(contents, "CPU part")?;
    Some(format!("ARM implementer {implementer} part {part}"))
}

#[cfg(any(target_os = "linux", test))]
fn field<'a>(contents: &'a str, expected: &str) -> Option<&'a str> {
    contents.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        let value = value.trim();
        (name.trim() == expected && !value.is_empty()).then_some(value)
    })
}

fn collect_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let contents = fs::read_to_string("/proc/meminfo").ok()?;
        let kibibytes = contents.lines().find_map(|line| {
            let value = line.strip_prefix("MemTotal:")?.trim();
            value.split_whitespace().next()?.parse::<u64>().ok()
        })?;
        kibibytes.checked_mul(1_024)
    }
    #[cfg(target_os = "macos")]
    {
        let output = command_text(
            Command::new("sysctl").args(["-n", "hw.memsize"]),
            "total_memory",
        )
        .ok()?;
        output.parse().ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

fn command_text(command: &mut Command, label: &'static str) -> Result<String, EnvironmentError> {
    let output = command_output(command, label)?;
    let value = String::from_utf8(output.stdout).map_err(|error| EnvironmentError::Command {
        command: label,
        detail: bounded(&error.to_string()),
    })?;
    let value = value.trim();
    if value.is_empty() {
        Err(EnvironmentError::Command {
            command: label,
            detail: "command returned empty output".to_owned(),
        })
    } else {
        Ok(bounded(value))
    }
}

fn command_output(command: &mut Command, label: &'static str) -> Result<Output, EnvironmentError> {
    let output = command
        .output()
        .map_err(|error| EnvironmentError::Command {
            command: label,
            detail: bounded(&error.to_string()),
        })?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(EnvironmentError::Command {
            command: label,
            detail: bounded(&String::from_utf8_lossy(&output.stderr)),
        })
    }
}

fn invalid(field: &'static str, detail: &'static str) -> EnvironmentError {
    EnvironmentError::InvalidField { field, detail }
}

fn bounded(value: &str) -> String {
    let mut end = value.len().min(MAX_COMMAND_OUTPUT);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::parse_cpu_model;

    #[test]
    fn parses_native_cpu_model_without_architecture_specific_placeholders() {
        assert_eq!(
            parse_cpu_model("Architecture: aarch64\nModel name: Neoverse-N1\n"),
            Some("Neoverse-N1".to_owned())
        );
        assert_eq!(
            parse_cpu_model("CPU implementer : 0x41\nCPU part : 0xd0c\n"),
            Some("ARM implementer 0x41 part 0xd0c".to_owned())
        );
    }
}
