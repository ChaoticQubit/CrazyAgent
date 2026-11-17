#[cfg(windows)]
use super::{INTERPRETER_DEPTH_MAX, Kind, Missing, Places, Refusal, Resolved, entry_at};
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
/// file's text. Output: the target's parts, or none.
fn cmd_shim_target(text: &str) -> Option<Vec<&str>> {
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
    (plain && text == shim_text(program, target)).then_some(parts)
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

/// How a batch file the resolver found runs on Windows, where std would start it through
/// cmd.exe: an npm cmd-shim as the target it names, which is then its program and pin;
/// npm's own `npx.cmd` as the `node.exe` and `npx-cli.js` beside it, read by where they sit
/// and never by its text (leader 04:09); any other `.cmd` or `.bat` refused. A shim's
/// target and npx's files pass every check an entry passes ([`entry_at`]), one level
/// deeper, so a batch file leading to another stays bounded. Inputs: the entry, its checked
/// real file, the places, the depth and how a refusal reads. Output: none when the real
/// file is no batch file, else the tool or why it cannot run.
#[cfg(windows)]
pub(super) fn entry(
    entry: &Path,
    program: &Path,
    places: &Places,
    depth: usize,
    refused: impl Fn(&Path, Refusal) -> Missing,
) -> Option<Result<Resolved, Missing>> {
    if !batch_file(program) {
        return None;
    }
    let refuse = || Some(Err(refused(program, Refusal::Batch)));
    let Some(folder) = program.parent().filter(|_| depth < INTERPRETER_DEPTH_MAX) else {
        return refuse();
    };
    if let Some((node, cli)) = npx_layout(program, folder) {
        let node = match entry_at(&node, places, depth + 1) {
            Ok(node) if node.kind == Kind::Native => node,
            Ok(_) => return refuse(),
            Err(missing) => return Some(Err(missing)),
        };
        return Some(entry_at(&cli, places, depth + 1).map(|script| Resolved {
            entry: entry.to_path_buf(),
            program: script.program,
            kind: Kind::Script {
                interpreter: Box::new(node),
                arguments: Vec::new(),
                by_name: false,
            },
        }));
    }
    let Some(target) = shim_target(program) else {
        return refuse();
    };
    Some(entry_at(&target, places, depth + 1).map(|target| Resolved {
        entry: entry.to_path_buf(),
        program: target.program,
        kind: target.kind,
    }))
}

/// Whether Windows would start a file through cmd.exe: a `.cmd` or `.bat` file, in any case.
/// Input: the file. Output: the answer.
#[cfg(windows)]
pub(crate) fn batch_file(file: &Path) -> bool {
    file.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

/// The file an npm cmd-shim starts, below its own folder as the shim names it, links kept.
/// Input: the shim's real file. Output: the target, none for any other file.
#[cfg(windows)]
fn shim_target(cmd: &Path) -> Option<PathBuf> {
    let folder = cmd.parent()?;
    let text = shim_file_text(cmd)?;
    let parts = cmd_shim_target(&text)?;
    assert!(!parts.is_empty());
    Some(
        parts
            .iter()
            .fold(folder.to_path_buf(), |at, part| at.join(part)),
    )
}

/// The real file an npm cmd-shim starts, which is the pin of a harness found as one, as the
/// real file of npm's link is on Unix. npx and any other batch file give none. Input: the
/// shim's real file. Output: the target's real file.
#[cfg(windows)]
pub(crate) fn cmd_shim_real(cmd: &Path) -> Option<PathBuf> {
    shim_target(cmd)?.canonicalize().ok()
}

/// npm's own `npx.cmd` by where it sits: named `npx.cmd`, with `node.exe` beside it and the
/// `node_modules\npm\bin\npx-cli.js` npm ships below that same folder. Inputs: the real
/// `.cmd` file and its folder. Output: node and the script, none for any other file.
#[cfg(windows)]
fn npx_layout(cmd: &Path, folder: &Path) -> Option<(PathBuf, PathBuf)> {
    let named = cmd
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case("npx.cmd"));
    let node = folder.join("node.exe");
    let cli = folder
        .join("node_modules")
        .join("npm")
        .join("bin")
        .join("npx-cli.js");
    (named && node.is_file() && cli.is_file()).then_some((node, cli))
}

/// The text of a batch file, read up to one byte past the bound a cmd-shim can reach.
/// Input: the file. Output: the text, none when it cannot be read, is not UTF-8 or is too
/// long.
#[cfg(windows)]
fn shim_file_text(file: &Path) -> Option<String> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(file)
        .ok()?
        .take(SHIM_BYTES_MAX as u64 + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() <= SHIM_BYTES_MAX).then_some(text)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(in crate::tools) const CODEX: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n";
    const NATIVE: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\"%dp0%\\node_modules\\native\\bin\\tool.exe\"   %*\r\n";

    /// V243-WIN-HARNESS-SEARCH: the `.cmd` npm 11's cmd-shim 8.0.0 wrote for Codex (made with
    /// the real generator, byte for byte) names the package's script below the shim's
    /// folder.
    #[test]
    fn an_npm_cmd_shim_names_the_script_it_starts() {
        assert_eq!(
            cmd_shim_target(CODEX),
            Some(vec!["node_modules", "@openai", "codex", "bin", "codex.js"])
        );
        assert_eq!(
            shim_text("node", r"node_modules\@openai\codex\bin\codex.js"),
            CODEX
        );
    }

    /// Any text but the generator's exact layout names nothing: line ends changed, a line
    /// added, a setting, arguments, the layout for a native target, a program given by path,
    /// or a target that climbs, has a `%`, a quote, a drive or a part Windows trims.
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
