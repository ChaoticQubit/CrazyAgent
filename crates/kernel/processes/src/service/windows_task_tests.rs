use super::markup::{WINDOWS_PATH_BYTES_MAX, windows_task_path};
use super::test_support::service;
use super::*;
use crate::update::test_support::{TestResult, at};

/// The Windows service at Windows paths, or at their Unix spelling on a host where a drive
/// path is not absolute, with the given PATH. Input: the PATH. Output: the service.
fn windows(path: &str) -> Service {
    let mut windows = service(Platform::Windows);
    windows.executable = PathBuf::from(r"C:\Users\owner\AppData\Local\CrazyAgent\crazyagent.exe");
    windows.root = PathBuf::from(r"C:\Users\owner\AppData\Local\CrazyAgent\engine");
    if !windows.executable.is_absolute() {
        windows.executable =
            PathBuf::from("/c/Users/owner/AppData/Local/CrazyAgent/crazyagent.exe");
        windows.root = PathBuf::from("/c/Users/owner/AppData/Local/CrazyAgent/engine");
    }
    windows.path = OsString::from(path);
    windows
}

/// The task's Arguments as cmd reads them, with the XML escapes undone. Input: the task
/// definition. Output: the line.
fn arguments(text: &str) -> String {
    let start = text.find("<Arguments>").unwrap() + "<Arguments>".len();
    let end = text.find("</Arguments>").unwrap();
    assert!(start <= end);
    text[start..end]
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

/// The Windows logon task sets the service PATH, its folders as the service gives them,
/// then runs the engine with its log redirection as before, as launchd and systemd give the
/// engine that PATH on macOS and Linux; with no PATH the line is the one without it.
#[test]
fn a_windows_task_sets_the_service_path_before_it_runs_the_engine() {
    let task = windows(r"C:\Tools\bin;C:\Users\owner\.local\bin;C:\Windows\System32");
    let executable = task.executable.to_string_lossy().into_owned();
    let root = task.root.to_string_lossy().into_owned();
    let log = task.root.join("logs").join("engine.log");
    let log = log.to_string_lossy();
    let run = format!(r#""{executable}" engine --home "{root}" >> "{log}" 2>&1"#);
    assert_eq!(
        arguments(&task.definition_text().unwrap()),
        format!(
            r#"/d /s /c "set "PATH=C:\Tools\bin;C:\Users\owner\.local\bin;C:\Windows\System32" && {run}""#
        )
    );
    assert_eq!(
        arguments(&windows("").definition_text().unwrap()),
        format!(r#"/d /s /c "{run}""#)
    );
}

/// Empty entries and folders holding `%`, which cmd expands even inside quotes, or `"` are
/// left out of the task's PATH; past [`WINDOWS_PATH_BYTES_MAX`] bytes the rest of the
/// folders are left out whole, and the task's line stays under cmd's 8191 characters.
#[test]
fn a_windows_task_path_leaves_out_folders_cmd_cannot_carry_and_stops_at_its_budget() {
    assert_eq!(
        windows_task_path(OsStr::new(r#"C:\a;C:\50%\bin;;C:\"q"\bin;C:\%PATH%;C:\b"#)),
        Some(r"C:\a;C:\b".to_owned())
    );
    for none in ["", ";", r"C:\50%", r#"C:\"q""#] {
        assert_eq!(windows_task_path(OsStr::new(none)), None, "{none}");
    }
    let folders: Vec<String> = (0..400)
        .map(|n| format!(r"C:\Tools\folder-{n:03}\bin"))
        .collect();
    let long = folders.join(";");
    let kept = windows_task_path(OsStr::new(&long)).unwrap();
    let count = kept.split(';').count();
    assert!(kept.len() <= WINDOWS_PATH_BYTES_MAX);
    assert!(count < folders.len());
    assert_eq!(kept, folders[..count].join(";"));
    assert!(kept.len() + 1 + folders[count].len() > WINDOWS_PATH_BYTES_MAX);
    let line = arguments(&windows(&long).definition_text().unwrap());
    assert!(line.contains(&format!("set \"PATH={kept}\" && ")));
    assert!(
        WINDOWS_SHELL.len() + 1 + line.encode_utf16().count() < CMD_LINE_CHARS_MAX,
        "{}",
        line.len()
    );
}

/// A task whose line would pass cmd's 8191 characters, as with a data folder path that long,
/// is refused with the limit named instead of written as a task cmd cannot run.
#[test]
fn a_windows_task_line_past_cmds_limit_is_refused() {
    let mut task = windows(r"C:\Tools\bin");
    task.root = task.root.join("x".repeat(CMD_LINE_CHARS_MAX));
    let refused = task.definition_text().unwrap_err();
    assert!(refused.0.contains("8191"), "{}", refused.0);
}

/// The installed Windows task is read in the UTF-16 it is written in and judged by the
/// engine it names: one naming this service's engine runs no other program, whatever PATH
/// it sets or whether it sets one, while one naming another engine does; with nothing
/// installed it runs none.
#[test]
fn a_written_windows_task_is_judged_by_the_engine_it_names() -> TestResult {
    let folder = tempfile::tempdir()?;
    let mut own = windows(r"C:\Tools\bin");
    own.definition = folder.path().join("crazyagent-engine-task.xml");
    let install = |service: &Service| -> TestResult {
        let text = service.definition_text().unwrap();
        write_definition(&service.definition, &text, Platform::Windows)?;
        Ok(())
    };
    install(&own)?;
    assert!(!own.runs_another_program());
    let mut other_path = own.clone();
    other_path.path = OsString::from(r"C:\Other\bin;C:\Tools\bin");
    assert!(!other_path.runs_another_program());
    let mut older = own.clone();
    older.path = OsString::new();
    install(&older)?;
    assert!(!own.runs_another_program());
    let mut other = own.clone();
    other.executable = other.executable.with_file_name("crazyagent-old.exe");
    assert!(other.runs_another_program());
    install(&other)?;
    assert!(own.runs_another_program());
    at(&own.definition, std::fs::remove_file(&own.definition))?;
    assert!(!own.runs_another_program());
    Ok(())
}
