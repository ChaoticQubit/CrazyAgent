use super::*;

/// Runs a service manager query. A manager that is not on this computer, such as `systemctl`
/// on a host without systemd, has nothing installed or running, so it reads as a failed
/// query. Input: the command. Output: whether it succeeded and its standard output.
pub(super) fn run_quiet(command: &[OsString]) -> Result<(bool, String), ServiceError> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| failure("empty service command"))?;
    let output = match crate::spawn_gate::output(
        std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    ) {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((false, String::new()));
        }
        Err(error) => return Err(failure(format!("{}: {error}", program.to_string_lossy()))),
    };
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    ))
}

pub(super) fn run_checked(command: &[OsString]) -> Result<(), ServiceError> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| failure("empty service command"))?;
    run_process(std::process::Command::new(program).args(args))
}

/// Runs one service command to its end with no input, capturing its output. Input: the
/// command. Output: Ok on success, else an error showing its program, arguments, status
/// and error text, never its environment.
fn run_process(process: &mut std::process::Command) -> Result<(), ServiceError> {
    let program = process.get_program().to_string_lossy().into_owned();
    let output = crate::spawn_gate::output(
        process
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .map_err(|error| failure(format!("{program}: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    let shown: Vec<_> = std::iter::once(process.get_program())
        .chain(process.get_args())
        .map(|part| part.to_string_lossy())
        .collect();
    let detail = String::from_utf8_lossy(&output.stderr);
    Err(failure(format!(
        "`{}` failed ({}): {}",
        shown.join(" "),
        output.status,
        detail.trim()
    )))
}
