use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn codex() -> PathBuf {
    let path = std::env::var_os("CRAZYAGENT_CODEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/homebrew/Caskroom/codex/0.163.0/bin/codex"));
    assert!(
        path.is_file(),
        "real codex binary required: {}",
        path.display()
    );
    path
}

#[test]
fn the_binary_runs_research_for_a_project_and_prints_plain_progress() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let state = root.join("state");
    fs::create_dir_all(project.join("context")).unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::write(
        project.join("context").join("brief.md"),
        "# Plant swap\n\nA mobile app where neighbours swap house plants.\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_crazyagent"))
        .arg("research")
        .arg(&state)
        .arg("project-plants")
        .arg(&project)
        .arg("--codex-executable")
        .arg(codex())
        .args(["--endpoint", "http://127.0.0.1:11434/v1"])
        .args(["--model", "qwen2.5:0.5b"])
        .args(["--minutes", "10"])
        .arg("competitor_research")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("Competitor research: Running"), "{stdout}");
    let report = project
        .join("context")
        .join("research")
        .join("competitor_research.md");
    assert!(
        stdout.contains(&format!("Competitor research: Done - {}", report.display())),
        "{stdout}"
    );
    let text = fs::read_to_string(&report).unwrap();
    assert!(text.starts_with("# Competitor research\n"), "{text}");
    assert!(text.contains("## Links and references"), "{text}");
    assert!(!stdout.contains("runner-"), "{stdout}");
}

#[test]
fn the_binary_refuses_bad_research_arguments_plainly() {
    let refused = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    assert!(refused(&["research"]).starts_with("usage: crazyagent research"));
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_str().unwrap();
    let stderr = refused(&[
        "research",
        dir,
        "p",
        dir,
        "--codex-executable",
        "/bin/sh",
        "--endpoint",
        "http://127.0.0.1:9/v1",
        "--model",
        "m",
        "../escape",
    ]);
    assert_eq!(stderr.trim(), "research kind refused: ../escape");
}
