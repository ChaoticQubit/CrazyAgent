use kernel_sandbox::policy::AgentPolicy;
use kernel_sandbox::profile::ProjectSandbox;
use kernel_sandbox::tool_cache::{CacheShape, PRIMING_ENV, UV, key, prepare};
use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Gives a launch's sandbox the tool cache its project's policy grants (T8): the cache
/// [`key`] names under the engine runtime for the project, prepared with the uv the agent's
/// PATH finds when the engine may run it. A refusal is logged with its code and the sandbox
/// goes on without a cache, so uv keeps its own default, which the SM1-R rule refuses
/// (fail closed). Inputs: the sandbox, the engine runtime, the canonical project folder, the
/// policy, the shape and the agent. Output: the sandbox, with the cache when granted.
pub fn grant(
    sandbox: ProjectSandbox,
    runtime: &Path,
    project: &Path,
    policy: &AgentPolicy,
    shape: CacheShape,
    agent: &str,
) -> ProjectSandbox {
    let Some(key) = key(policy.tool_cache, policy.access, shape, agent) else {
        return sandbox;
    };
    let Some(project) = project.to_str() else {
        eprintln!("agent_tool_cache_unavailable code=tool_cache_project_name");
        return sandbox;
    };
    let uv = sandbox.tool_program(UV);
    let prepared = match prepare(
        &super::TOOL_CACHE_HOOKS,
        runtime,
        project,
        &key,
        uv.as_deref(),
    ) {
        Ok(prepared) => prepared,
        Err(code) => {
            eprintln!("agent_tool_cache_unavailable code={code}");
            return sandbox;
        }
    };
    match sandbox
        .clone()
        .allow_tool_cache(&prepared.root, prepared.uv)
    {
        Ok(granted) => granted,
        Err(_) => {
            eprintln!("agent_tool_cache_unavailable code=tool_cache_root_refused");
            sandbox
        }
    }
}

/// Builds the priming process: uv's `tool list`, which makes the cache and its markers and
/// nothing more, under [`kernel_sandbox::profile::seatbelt::priming_profile`], from a cleared environment holding only
/// [`PRIMING_ENV`], in its own process group with no input or output. Inputs: the priming
/// folder and the canonical uv binary. Output: the unspawned command.
pub fn priming_command(folder: &Path, uv: &Path) -> Command {
    let mut command = Command::new(kernel_sandbox::profile::command::SANDBOX_EXEC);
    command
        .arg("-p")
        .arg(kernel_sandbox::profile::seatbelt::priming_profile(
            folder, uv,
        ))
        .arg(uv)
        .args(["tool", "list"])
        .env_clear()
        .env("HOME", folder.join("home"))
        .env("UV_CACHE_DIR", folder.join("cache"))
        .env("UV_NO_CONFIG", "1")
        .env("UV_OFFLINE", "1")
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut names: Vec<&OsStr> = command.get_envs().map(|(name, _)| name).collect();
    names.sort();
    let mut listed: Vec<&OsStr> = PRIMING_ENV.iter().map(OsStr::new).collect();
    listed.sort();
    assert_eq!(names, listed);
    command
}

#[cfg(test)]
mod tests;
