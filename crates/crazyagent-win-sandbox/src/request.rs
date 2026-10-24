use crate::profile::check_path;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// The flag before the boundary profile path.
pub const PROFILE_FLAG: &str = "--profile";
/// The marker between the helper's own arguments and the agent's program.
pub const SEPARATOR: &str = "--";
/// The exit code the helper returns when it refuses to start the agent.
pub const REFUSED_EXIT: u8 = 125;
/// The most arguments the helper passes to the agent.
pub const ARGS_MAX: usize = 1024;
/// How the helper is called, shown when its arguments are wrong.
pub const USAGE: &str = "usage: crazyagent-win-boundary --profile <file> -- <program> [arguments]";

/// One launch the helper was asked for: the profile file, the agent's program as a plain drive
/// path, and the program's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub profile: PathBuf,
    pub program: String,
    pub args: Vec<OsString>,
}

/// Reads the helper's arguments, without the helper's own name: `--profile <file> -- <program>
/// [arguments]`. The profile and program must be plain drive paths, and batch files are refused
/// because Windows runs them through `cmd.exe` with its own parsing. Input: the arguments.
/// Output: the request, or the plain reason it was refused.
pub fn parse(args: Vec<OsString>) -> Result<Request, &'static str> {
    let mut args = args.into_iter();
    if args.next().as_deref() != Some(OsStr::new(PROFILE_FLAG)) {
        return Err(USAGE);
    }
    let profile = args.next().ok_or(USAGE)?;
    if args.next().as_deref() != Some(OsStr::new(SEPARATOR)) {
        return Err(USAGE);
    }
    let program = args.next().ok_or(USAGE)?;
    let rest: Vec<OsString> = args.collect();
    if rest.len() > ARGS_MAX {
        return Err("the agent has more than 1024 arguments");
    }
    let profile = profile
        .into_string()
        .map_err(|_| "the profile path is not Unicode text")?;
    check_path(&profile)?;
    let program = program
        .into_string()
        .map_err(|_| "the program path is not Unicode text")?;
    check_path(&program)?;
    let lower = program.to_ascii_lowercase();
    if lower.ends_with(".bat") || lower.ends_with(".cmd") {
        return Err("batch files are refused; start cmd.exe with /C instead");
    }
    Ok(Request {
        profile: PathBuf::from(profile),
        program,
        args: rest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_well_formed_call_names_the_profile_program_and_arguments() {
        let request = parse(args(&[
            "--profile",
            r"C:\Users\mira\AppData\Local\CrazyAgent\profiles\a1.profile",
            "--",
            r"C:\Windows\System32\cmd.exe",
            "/C",
            "echo hi",
            "",
        ]))
        .unwrap();
        assert_eq!(
            request.profile,
            PathBuf::from(r"C:\Users\mira\AppData\Local\CrazyAgent\profiles\a1.profile")
        );
        assert_eq!(request.program, r"C:\Windows\System32\cmd.exe");
        assert_eq!(request.args, args(&["/C", "echo hi", ""]));
        let bare = parse(args(&[
            "--profile",
            r"C:\p\a.profile",
            "--",
            r"C:\bin\tool.exe",
        ]))
        .unwrap();
        assert!(bare.args.is_empty());
    }

    #[test]
    fn missing_pieces_relative_paths_and_batch_files_are_refused() {
        let profile = r"C:\p\a.profile";
        let program = r"C:\bin\tool.exe";
        assert_eq!(parse(args(&[])), Err(USAGE));
        assert_eq!(parse(args(&["--profile"])), Err(USAGE));
        assert_eq!(parse(args(&["--profile", profile, program])), Err(USAGE));
        assert_eq!(parse(args(&["--profile", profile, "--"])), Err(USAGE));
        assert_eq!(
            parse(args(&["--policy", profile, "--", program])),
            Err(USAGE)
        );
        assert!(parse(args(&["--profile", "a.profile", "--", program])).is_err());
        assert!(parse(args(&["--profile", profile, "--", "tool.exe"])).is_err());
        assert!(
            parse(args(&[
                "--profile",
                profile,
                "--",
                r"\\server\share\tool.exe"
            ]))
            .is_err()
        );
        for batch in [r"C:\bin\run.bat", r"C:\bin\RUN.CMD"] {
            assert_eq!(
                parse(args(&["--profile", profile, "--", batch])),
                Err("batch files are refused; start cmd.exe with /C instead")
            );
        }
        let mut many = args(&["--profile", profile, "--", program]);
        many.extend((0..=ARGS_MAX).map(|n| OsString::from(n.to_string())));
        assert_eq!(parse(many), Err("the agent has more than 1024 arguments"));
    }

    #[cfg(unix)]
    #[test]
    fn a_program_path_that_is_not_unicode_is_refused() {
        use std::os::unix::ffi::OsStringExt;
        let mut list = args(&["--profile", r"C:\p\a.profile", "--"]);
        list.push(OsString::from_vec(vec![b'C', b':', b'\\', 0xff]));
        assert_eq!(parse(list), Err("the program path is not Unicode text"));
    }

    #[cfg(windows)]
    #[test]
    fn a_program_path_that_is_not_unicode_is_refused() {
        use std::os::windows::ffi::OsStringExt;
        let mut list = args(&["--profile", r"C:\p\a.profile", "--"]);
        list.push(OsString::from_wide(&[
            b'C' as u16,
            b':' as u16,
            b'\\' as u16,
            0xd800,
        ]));
        assert_eq!(parse(list), Err("the program path is not Unicode text"));
    }
}
