use super::{INTERPRETER_DEPTH_MAX, Kind, Missing, Places, Refusal, Resolved, entry_at};
use std::path::Path;

/// How a batch file the resolver found runs on Windows, where std would start it through
/// cmd.exe: an npm cmd-shim as the target it names, which is then its program and pin;
/// npm's own `npx.cmd` as the `node.exe` and `npx-cli.js` beside it, read by where they sit
/// and never by its text (leader 04:09); any other `.cmd` or `.bat` refused. A shim's
/// target and npx's files pass every check an entry passes ([`entry_at`]), one level
/// deeper, so a batch file leading to another stays bounded; `npx-cli.js` looks its `node`
/// up beside npx.cmd only, so npx runs off the engine's PATH too (#341). Inputs: the entry,
/// its checked real file, the places, the depth and how a refusal reads. Output: none when
/// the real file is no batch file, else the tool or why it cannot run. It moves into
/// kernel/processes with the program finding core in ticket C2 (POD-C).
#[cfg(windows)]
pub(super) fn entry(
    entry: &Path,
    program: &Path,
    places: &Places,
    depth: usize,
    refused: impl Fn(&Path, Refusal) -> Missing,
) -> Option<Result<Resolved, Missing>> {
    if !kernel_processes::batch::batch_file(program) {
        return None;
    }
    let refuse = || Some(Err(refused(program, Refusal::Batch)));
    let Some(folder) = program.parent().filter(|_| depth < INTERPRETER_DEPTH_MAX) else {
        return refuse();
    };
    if let Some((node, cli)) = kernel_processes::batch::npx_layout(program, folder) {
        let node = match entry_at(&node, places, depth + 1) {
            Ok(node) if node.kind == Kind::Native => node,
            Ok(_) => return refuse(),
            Err(missing) => return Some(Err(missing)),
        };
        let beside = Places {
            folders: vec![folder.to_path_buf()],
            ..places.clone()
        };
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
    let Some(target) = kernel_processes::batch::shim_target(program) else {
        return refuse();
    };
    Some(entry_at(&target, places, depth + 1).map(|target| Resolved {
        entry: entry.to_path_buf(),
        program: target.program,
        kind: target.kind,
    }))
}
