#[cfg(windows)]
use std::ffi::OsStr;
#[cfg(windows)]
use std::path::{Path, PathBuf};

const SHIM_BYTES_MAX: usize = 4096;
const TARGET_BYTES_MAX: usize = 1024;
const PROGRAM_BYTES_MAX: usize = 64;
const SHIM_HEAD: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\";
const SHIM_RUN: &str =
    "endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\";
const SHIM_END: &str = "\" %*\r\n";

/// The file an npm cmd-shim `.cmd` starts, as parts below the shim's folder. Only the text
/// cmd-shim 8.0.0 writes for a target whose first line names its program by name, with no
/// arguments and no settings, is read: it is rendered again from the program and target it
/// names and compared byte for byte, so any other batch file names nothing. Input: the
/// file's text. Output: the program's name and the target's parts, or none.
fn cmd_shim_target(text: &str) -> Option<(&str, Vec<&str>)> {
    if text.len() > SHIM_BYTES_MAX {
        return None;
    }
    let (program, _) = text.strip_prefix(SHIM_HEAD)?.split_once(".exe\"")?;
    let (_, last) = text.split_once(SHIM_RUN)?;
    let target = last.strip_suffix(SHIM_END)?;
    let parts: Vec<&str> = target.split('\\').collect();
    let plain = program.len() <= PROGRAM_BYTES_MAX
        && plain_part(program)
        && target.len() <= TARGET_BYTES_MAX
        && parts.iter().all(|part| plain_part(part));
    (plain && text == shim_text(program, target)).then_some((program, parts))
}

/// The exact `.cmd` cmd-shim 8.0.0 writes for a target run by a program named on its first
/// line. Inputs: the program name and the target below the shim's folder. Output: the text.
fn shim_text(program: &str, target: &str) -> String {
    format!(
        "{SHIM_HEAD}{program}.exe\" (\r\n  SET \"_prog=%dp0%\\{program}.exe\"\r\n) ELSE (\r\n  SET \"_prog={program}\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\n{SHIM_RUN}{target}{SHIM_END}"
    )
}

/// Whether a name is one plain part of a Windows path: not empty, no `%` or quote, none of
/// the characters Windows refuses in a name, no control character, and not ending in a dot
/// or space, which Windows drops, so `.` and `..` are never parts. Input: the part.
/// Output: the answer.
fn plain_part(part: &str) -> bool {
    !part.is_empty()
        && !part.ends_with(['.', ' '])
        && part
            .chars()
            .all(|c| !c.is_control() && !"%\"*?<>|:/\\".contains(c))
}

/// Whether Windows would start a file through cmd.exe ([`batch_name`]). Input: the file.
/// Output: the answer, false for a path with no file name.
#[cfg(windows)]
pub fn batch_file(file: &Path) -> bool {
    file.file_name()
        .is_some_and(|name| batch_name(&name.to_string_lossy()))
}

/// Whether a program names a file no other local user can plant: an absolute path, or a bare
/// file name Windows looks up only where it searches. A root-relative (`\x`), drive-relative
/// (`C:x`) or folder-relative (`sub\x`, `.\x`) program resolves against a drive or folder the
/// engine did not choose. Input: the program. Output: the answer, false for an empty one.
#[cfg(windows)]
pub fn fixed_program(program: &Path) -> bool {
    let mut parts = program.components();
    program.is_absolute()
        || matches!(
            (parts.next(), parts.next()),
            (Some(std::path::Component::Normal(_)), None)
        )
}

/// Whether a file name is a batch file as Windows reads it: `.cmd` or `.bat` in any case once
/// the trailing dots and spaces Windows drops are gone, or any name with a `:` stream part,
/// which no program the engine starts has, so it fails closed. Input: the name. Output: the
/// answer.
fn batch_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let name = name.trim_end_matches(['.', ' ']);
    name.contains(':') || name.ends_with(".cmd") || name.ends_with(".bat")
}

/// Whether Windows starts a file directly as a program, never through cmd.exe
/// ([`program_name`]). Input: the file. Output: the answer, false for a path with no file
/// name.
#[cfg(windows)]
pub fn program_file(file: &Path) -> bool {
    file.file_name()
        .is_some_and(|name| program_name(&name.to_string_lossy()))
}

/// Whether a file name is one Windows starts directly as a program: `.exe` or `.com` in any
/// case once the trailing dots and spaces Windows drops are gone, with no `:` stream part. A
/// name with no extension or another one is not, since a launcher may complete it from
/// `PATHEXT` with a `.cmd` beside it. Input: the name. Output: the answer.
fn program_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let name = name.trim_end_matches(['.', ' ']);
    !name.contains(':') && (name.ends_with(".exe") || name.ends_with(".com"))
}

/// The file an npm cmd-shim starts, below its own folder as the shim names it, links kept.
/// Input: the shim's real file. Output: the target, none for any other file.
#[cfg(windows)]
pub fn shim_target(cmd: &Path) -> Option<PathBuf> {
    shim_parts(cmd).map(|(_, target)| target)
}

/// The file an npm cmd-shim starts, read from the shim already open, held by the caller,
/// no further than one byte past the bound a cmd-shim can reach, as the parts it names
/// joined below the shim's folder, links kept. Inputs: the shim's folder and the open shim.
/// Output: the target, none for any other text.
#[cfg(windows)]
pub fn held_shim_target(folder: &Path, shim: impl std::io::Read) -> Option<PathBuf> {
    let text = bounded_text(shim)?;
    let (_, parts) = cmd_shim_target(&text)?;
    assert!(!parts.is_empty());
    Some(
        parts
            .iter()
            .fold(folder.to_path_buf(), |at, part| at.join(part)),
    )
}

/// The program an npm cmd-shim prefers, `<program>.exe` beside the shim, and the file it
/// starts below that folder, links kept. Input: the shim's real file. Output: both, none
/// for any other file.
#[cfg(windows)]
fn shim_parts(cmd: &Path) -> Option<(PathBuf, PathBuf)> {
    let folder = cmd.parent()?;
    let text = shim_file_text(cmd)?;
    let (program, parts) = cmd_shim_target(&text)?;
    assert!(!parts.is_empty());
    assert!(plain_part(program));
    let target = parts
        .iter()
        .fold(folder.to_path_buf(), |at, part| at.join(part));
    Some((folder.join(format!("{program}.exe")), target))
}

/// The real file an npm cmd-shim starts, which is the pin of a harness found as one, as the
/// real file of npm's link is on Unix. npx, any other batch file and a shim whose target is
/// itself a batch file give none. Input: the shim's real file. Output: the target's real
/// file.
#[cfg(windows)]
pub fn cmd_shim_real(cmd: &Path) -> Option<PathBuf> {
    shim_target(cmd)?
        .canonicalize()
        .ok()
        .filter(|real| !batch_file(real))
}

/// npm's own `npm.cmd` and `npx.cmd` by where they sit: `node.exe` beside the file and, below
/// that same folder, the script npm ships for that name, `node_modules\npm\bin\npm-cli.js`
/// or `npx-cli.js`. Inputs: the real `.cmd` file and its folder. Output: node and the script,
/// none for any other name or a missing file.
#[cfg(windows)]
pub fn npm_layout(cmd: &Path, folder: &Path) -> Option<(PathBuf, PathBuf)> {
    let name = cmd.file_name().and_then(OsStr::to_str)?;
    let script = [("npm.cmd", "npm-cli.js"), ("npx.cmd", "npx-cli.js")]
        .into_iter()
        .find(|(command, _)| name.eq_ignore_ascii_case(command))?
        .1;
    let node = folder.join("node.exe");
    let cli = folder
        .join("node_modules")
        .join("npm")
        .join("bin")
        .join(script);
    (node.is_file() && cli.is_file()).then_some((node, cli))
}

/// npm's own `npx.cmd` as the real `node.exe` and `npx-cli.js` its layout names
/// ([`npm_layout`]), so a caller can tell the resolver mapped a batch file by that layout and
/// by no shim text. Input: the batch file as written. Output: both real files, none for any
/// other file, `npm.cmd` included.
#[cfg(windows)]
pub fn npx_real(cmd: &Path) -> Option<(PathBuf, PathBuf)> {
    let cmd = cmd.canonicalize().ok()?;
    let npx = cmd
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case("npx.cmd"));
    if !npx {
        return None;
    }
    let (node, cli) = npm_layout(&cmd, cmd.parent()?)?;
    Some((node.canonicalize().ok()?, cli.canonicalize().ok()?))
}

/// The text of a batch file, read up to one byte past the bound a cmd-shim can reach.
/// Input: the file. Output: the text, none when it cannot be read, is not UTF-8 or is too
/// long.
#[cfg(windows)]
fn shim_file_text(file: &Path) -> Option<String> {
    bounded_text(std::fs::File::open(file).ok()?)
}

/// The text of an open batch file, read up to one byte past the bound a cmd-shim can
/// reach. Input: the reader. Output: the text, none when it cannot be read, is not UTF-8 or
/// is too long.
#[cfg(windows)]
fn bounded_text(file: impl std::io::Read) -> Option<String> {
    use std::io::Read;
    let mut text = String::new();
    file.take(SHIM_BYTES_MAX as u64 + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() <= SHIM_BYTES_MAX).then_some(text)
}

/// Test text: the `.cmd` npm 11's cmd-shim 8.0.0 writes for Codex, byte for byte, shared
/// with the engine's Windows program finding rows. Output: the shim text.
#[cfg(any(test, all(windows, feature = "test-support")))]
pub const CODEX: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n";

#[cfg(windows)]
use crate::finding::{INTERPRETER_DEPTH_MAX, Kind, Missing, Places, Refusal, Resolved, entry_at};

/// How a batch file the resolver found runs on Windows, where std would start it through
/// cmd.exe: an npm cmd-shim as the target it names, which is then its program and pin, run
/// by the `<program>.exe` beside the shim when one is there, as the shim's text prefers it,
/// and a refused one is refused, never passed over for one on PATH;
/// npm's own `npm.cmd` and `npx.cmd` as the `node.exe` and npm script beside them, read by
/// where they sit and never by their text; any other `.cmd` or `.bat` refused. A shim's
/// target and npm's files pass every check an entry passes ([`entry_at`]), one level
/// deeper, so a batch file leading to another stays bounded; npm's scripts look their
/// `node` up beside the `.cmd` only, so npm and npx run off the engine's PATH too. Inputs: the entry,
/// its checked real file, the places, the depth and how a refusal reads. Output: none when
/// the real file is no batch file, else the tool or why it cannot run.
#[cfg(windows)]
pub(crate) fn entry(
    entry: &Path,
    program: &Path,
    places: &Places,
    depth: usize,
    refused: impl Fn(&Path, Refusal) -> Missing,
) -> Option<Result<Resolved, Missing>> {
    if !crate::batch::batch_file(program) {
        return None;
    }
    let refuse = || Some(Err(refused(program, Refusal::Batch)));
    let Some(folder) = program.parent().filter(|_| depth < INTERPRETER_DEPTH_MAX) else {
        return refuse();
    };
    if let Some((node, cli)) = crate::batch::npm_layout(program, folder) {
        let node = match entry_at(&node, places, depth + 1) {
            Ok(node) if node.kind == Kind::Native => node,
            Ok(_) => return refuse(),
            Err(missing) => return Some(Err(missing)),
        };
        let beside = places.only_in(folder);
        return Some(entry_at(&cli, &beside, depth + 1).map(|script| Resolved {
            entry: entry.to_path_buf(),
            program: script.program,
            kind: Kind::Script {
                interpreter: Box::new(node),
                arguments: Vec::new(),
                by_name: false,
            },
        }));
    }
    let Some((beside, target)) = crate::batch::shim_parts(program) else {
        return refuse();
    };
    if beside.is_file() {
        let interpreter = match entry_at(&beside, places, depth + 1) {
            Ok(interpreter) if interpreter.kind == Kind::Native => interpreter,
            Ok(_) => return refuse(),
            Err(missing) => return Some(Err(missing)),
        };
        let only_beside = places.only_in(folder);
        return Some(
            entry_at(&target, &only_beside, depth + 1).map(|script| Resolved {
                entry: entry.to_path_buf(),
                program: script.program,
                kind: Kind::Script {
                    interpreter: Box::new(interpreter),
                    arguments: Vec::new(),
                    by_name: false,
                },
            }),
        );
    }
    Some(entry_at(&target, places, depth + 1).map(|target| Resolved {
        entry: entry.to_path_buf(),
        program: target.program,
        kind: target.kind,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NATIVE: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\"%dp0%\\node_modules\\native\\bin\\tool.exe\"   %*\r\n";

    /// The `.cmd` npm 11's cmd-shim 8.0.0 wrote for Codex (made with the real generator, byte
    /// for byte) names the package's script below the shim's folder.
    #[test]
    fn an_npm_cmd_shim_names_the_script_it_starts() {
        assert_eq!(
            cmd_shim_target(CODEX),
            Some((
                "node",
                vec!["node_modules", "@openai", "codex", "bin", "codex.js"]
            ))
        );
        assert_eq!(
            shim_text("node", r"node_modules\@openai\codex\bin\codex.js"),
            CODEX
        );
    }

    /// An open cmd-shim is read no further than one byte past the bound a cmd-shim can
    /// reach: the text npm writes names its target joined below the folder given, and a
    /// longer file names nothing after exactly that many bytes are read from it.
    #[cfg(windows)]
    #[test]
    fn an_open_shim_is_read_only_up_to_the_cmd_shim_bound() {
        use std::io::Read;
        let folder = Path::new(r"C:\engine\npm");
        assert_eq!(
            held_shim_target(folder, CODEX.as_bytes()),
            Some(folder.join(r"node_modules\@openai\codex\bin\codex.js"))
        );
        let offered = 10 * SHIM_BYTES_MAX as u64;
        let mut long = CODEX.as_bytes().chain(std::io::repeat(b' ')).take(offered);
        assert_eq!(held_shim_target(folder, &mut long), None);
        assert_eq!(long.limit(), offered - SHIM_BYTES_MAX as u64 - 1);
    }

    /// A file name is a batch file as Windows reads it: `.cmd` or `.bat` in any case with
    /// trailing dots and spaces dropped, and any name with a `:` stream part, even one whose
    /// part before the stream is a program.
    #[test]
    fn a_batch_name_is_read_as_windows_reads_it() {
        for name in [
            "run.cmd",
            "RUN.BAT",
            "run.Cmd.",
            "run.cmd ",
            "run.bat. .",
            "tool.exe:x.cmd",
            "tool.exe:stream",
            "tool.exe::$DATA",
        ] {
            assert!(batch_name(name), "{name}");
        }
        for name in ["tool.exe", "cmd", "run.cmdx", "run.cmd.exe", "bat", ""] {
            assert!(!batch_name(name), "{name}");
        }
    }

    /// Only `.exe` or `.com` in any case, trailing dots and spaces dropped, is a program
    /// Windows starts directly; a name with no extension, a dotted one such as `tool.v2`, a
    /// batch file, a script or any stream part is not.
    #[test]
    fn a_program_name_is_read_as_windows_reads_it() {
        for name in [
            "node.exe",
            "NODE.EXE",
            "tool.Com",
            "tool.exe.",
            "tool.exe ",
            "run.cmd.exe",
        ] {
            assert!(program_name(name), "{name}");
        }
        for name in [
            "tool",
            "tool.v2",
            "npx",
            "npx.cmd",
            "run.bat",
            "cli.js",
            "tool.exe:stream",
            "tool.exe::$DATA",
            "tool.exex",
            "exe",
            "",
        ] {
            assert!(!program_name(name), "{name}");
        }
    }

    /// Any text but the generator's exact layout names nothing: line ends changed, a line
    /// added or changed inside the layout, another program, a setting, arguments, the native
    /// target layout, a program given by path, or a target that climbs, has a `%`, a quote,
    /// a drive, an empty part, a part Windows trims or too many bytes.
    #[test]
    fn every_other_batch_text_names_nothing() {
        let codex_js = r"node_modules\@openai\codex\bin\codex.js";
        let changed = [
            CODEX.replace("\r\n", "\n"),
            CODEX.replace("CALL :find_dp0\r\n", "CALL :find_dp0\r\ncalc.exe\r\n"),
            CODEX.replace(
                "CALL :find_dp0\r\n\r\n",
                "CALL :find_dp0\r\n@SET \"A=1\"\r\n\r\n",
            ),
            CODEX.replace("\"%_prog%\"  ", "\"%_prog%\" --flag "),
            CODEX.replace("  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n", "  calc.exe\r\n"),
            CODEX.replace("SET \"_prog=node\"", "SET \"_prog=calc\""),
            CODEX.replace(" %*\r\n", " %*\r\ncalc.exe\r\n"),
            format!("{CODEX}calc.exe\r\n"),
            NATIVE.to_owned(),
            shim_text("/usr/bin/node", codex_js),
            shim_text(r"C:\node", codex_js),
            shim_text("node", r"..\evil.js"),
            shim_text("node", r"node_modules\..\..\evil.js"),
            shim_text("node", r"node_modules\%PATH%\x.js"),
            shim_text("node", "node_modules\\a\" & calc & \"\\x.js"),
            shim_text("node", r"C:\evil.js"),
            shim_text("node", r"node_modules\x.js."),
            shim_text("node", r"node_modules\\x.js"),
            shim_text(
                "node",
                &format!("{}x.js", "a\\".repeat(TARGET_BYTES_MAX / 2)),
            ),
            String::new(),
        ];
        for text in &changed {
            assert_eq!(cmd_shim_target(text), None, "{text:?}");
        }
    }
}
