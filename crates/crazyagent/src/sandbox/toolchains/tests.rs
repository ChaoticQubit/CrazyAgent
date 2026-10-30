use super::*;

#[test]
fn the_agent_git_script_is_refused_for_the_xcrun_shim_and_any_path_that_is_not_plain() {
    for git in [
        "/usr/bin/git",
        "usr/bin/git",
        "/opt/it's/git",
        "/opt/a\"b/git",
        "/opt/$HOME/git",
        "/opt/a`b`/git",
        "/opt/new\nline/git",
        "/opt/semi;colon/git",
    ] {
        assert_eq!(git_script(Path::new(git), false), None, "{git:?}");
        assert_eq!(git_script(Path::new(git), true), None, "{git:?}");
    }
    let worker = git_script(Path::new("/Applications/X code.app/usr/bin/git"), false).unwrap();
    let lead = git_script(Path::new("/Applications/X code.app/usr/bin/git"), true).unwrap();
    for (script, flag) in [(&worker, "lead=0"), (&lead, "lead=1")] {
        assert!(script.starts_with("#!/bin/sh\ngit='/Applications/X code.app/usr/bin/git'\n"));
        assert!(script.lines().any(|line| line == flag), "{flag}");
        assert!(!script.contains("@GIT@") && !script.contains("@LEAD@"));
        assert!(script.len() as u64 <= GIT_SCRIPT_BYTES_MAX);
    }
    assert_eq!(worker.replace("\nlead=0\n", "\nlead=1\n"), lead);
}
