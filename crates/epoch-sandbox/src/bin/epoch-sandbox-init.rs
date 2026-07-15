//! Trusted final launcher that installs the versioned seccomp allowlist before `execve`.

#[cfg(target_os = "linux")]
fn main() {
    if let Err(message) = linux::run() {
        eprintln!("epoch-sandbox-init: {message}");
        std::process::exit(125);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("epoch-sandbox-init: Linux seccomp is unsupported on this platform");
    std::process::exit(125);
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{env, os::unix::process::CommandExt as _, process::Command};

    use seccompiler::{BpfMap, TargetArch};

    pub fn run() -> Result<(), String> {
        let mut arguments = env::args_os().skip(1);
        if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--seccomp-profile-v1"))
            || arguments.next().as_deref() != Some(std::ffi::OsStr::new("--"))
        {
            return Err("expected --seccomp-profile-v1 -- <absolute-program> [args...]".to_owned());
        }
        let program = arguments
            .next()
            .ok_or_else(|| "missing workload executable".to_owned())?;
        if !std::path::Path::new(&program).is_absolute() {
            return Err("workload executable must be absolute".to_owned());
        }

        let architecture: TargetArch = env::consts::ARCH
            .try_into()
            .map_err(|error| format!("unsupported seccomp architecture: {error}"))?;
        let profile = profile_for_arch()?;
        let mut filters: BpfMap = seccompiler::compile_from_json(profile, architecture)
            .map_err(|error| format!("seccomp profile v1 did not compile: {error}"))?;
        let filter = filters
            .remove("epoch_agent")
            .ok_or_else(|| "seccomp profile v1 has no epoch_agent filter".to_owned())?;
        seccompiler::apply_filter(&filter)
            .map_err(|error| format!("seccomp profile v1 could not be installed: {error}"))?;

        let error = Command::new(program).args(arguments).exec();
        Err(format!("workload exec failed: {error}"))
    }

    fn profile_for_arch() -> Result<&'static [u8], String> {
        match env::consts::ARCH {
            "aarch64" => Ok(include_bytes!("seccomp-v1-aarch64.json")),
            "x86_64" => Ok(include_bytes!("seccomp-v1-x86_64.json")),
            architecture => Err(format!(
                "seccomp profile v1 is unavailable for {architecture}"
            )),
        }
    }
}
