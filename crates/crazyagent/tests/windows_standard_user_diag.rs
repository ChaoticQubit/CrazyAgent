#![cfg(windows)]

use crazyagent::harnesses::update::{self, Roots, Updates};
use crazyagent::harnesses::{self, HarnessEntry, Search};
use crazyagent_win_sandbox::private;
use std::fs::OpenOptions;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const JOB_TIME: Duration = Duration::from_secs(900);
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const TREE_LINES_MAX: usize = 400;
const TREE_DEPTH_MAX: usize = 7;
const MODULES_DEPTH_MAX: usize = 2;
const TAIL_LINES: usize = 40;
const SHIM_NAMES: [&str; 4] = ["codex", "codex.exe", "codex.cmd", "codex.ps1"];

/// Tells the waiting row that the install job let go of its signal, on every path a job ends by.
struct Ended(mpsc::Sender<()>);

impl Drop for Ended {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// The Codex row of the catalog for one engine root, with the engine's npm prefix searched last.
/// Input: the engine root. Output: the row.
fn codex(root: &Path) -> HarnessEntry {
    let search = Search::from_environment().with_engine(root);
    let found = harnesses::catalog(&search)
        .into_iter()
        .find(|entry| entry.id == "codex");
    found.expect("the catalog always lists codex")
}

/// Writes a binary security identifier in its S-1-... text form. Input: the identifier bytes.
/// Output: the text, or the bytes in hex when they are not a whole identifier.
fn sid_text(sid: &[u8]) -> String {
    let count = sid.get(1).copied().map_or(0, usize::from);
    if sid.len() < 8 || sid.len() != 8 + 4 * count {
        return format!("raw {sid:02x?}");
    }
    let authority = sid[2..8]
        .iter()
        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte));
    let mut text = format!("S-{}-{authority}", sid[0]);
    for part in sid[8..].chunks_exact(4) {
        let value = u32::from_le_bytes([part[0], part[1], part[2], part[3]]);
        text.push_str(&format!("-{value}"));
    }
    text
}

/// Prints the owner of a folder, read where it sits without following a link, and its icacls
/// list. Inputs: the folder and the running user's identifier.
fn folder_access(folder: &Path, user: &[u8]) {
    let opened = OpenOptions::new()
        .read(true)
        .custom_flags(BACKUP_SEMANTICS | OPEN_REPARSE_POINT)
        .open(folder);
    match opened
        .map_err(|error| error.to_string())
        .and_then(|file| private::facts(&file).map_err(|error| error.to_string()))
    {
        Ok(facts) => println!(
            "DIAG owner {} {} gate_user={}",
            sid_text(&facts.owner),
            folder.display(),
            facts.owner == user
        ),
        Err(error) => println!("DIAG owner unread {}: {error}", folder.display()),
    }
    match Command::new("icacls").arg(folder).output() {
        Ok(listed) => {
            for line in String::from_utf8_lossy(&listed.stdout).lines() {
                if !line.trim().is_empty() {
                    println!("DIAG icacls {line}");
                }
            }
        }
        Err(error) => println!("DIAG icacls failed {}: {error}", folder.display()),
    }
}

/// Lists a tree, one line per entry with its kind, attributes and size, at most
/// `TREE_DEPTH_MAX` deep and `MODULES_DEPTH_MAX` below any node_modules, at most
/// `TREE_LINES_MAX` lines in all. Inputs: the folder, its depth, its depth below node_modules,
/// the line count so far and the folders seen. Output: the folders listed, in the list.
fn tree(
    folder: &Path,
    depth: usize,
    below: Option<usize>,
    lines: &mut usize,
    seen: &mut Vec<PathBuf>,
) {
    seen.push(folder.to_path_buf());
    let Ok(read) = std::fs::read_dir(folder) else {
        println!("DIAG tree unreadable {}", folder.display());
        return;
    };
    let mut entries: Vec<PathBuf> = read
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();
    for path in entries {
        if *lines >= TREE_LINES_MAX {
            println!("DIAG tree cut at {TREE_LINES_MAX} lines");
            return;
        }
        *lines += 1;
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            println!("DIAG tree unstat {}", path.display());
            continue;
        };
        let kind = if meta.file_type().is_symlink() {
            "link"
        } else if meta.is_dir() {
            "dir"
        } else {
            "file"
        };
        println!(
            "DIAG tree {kind} attrs={:#x} bytes={} {}",
            meta.file_attributes(),
            meta.len(),
            path.display()
        );
        let modules = path.file_name().is_some_and(|name| name == "node_modules");
        let next = match below {
            Some(level) => Some(level + 1),
            None if modules => Some(0),
            None => None,
        };
        let deeper = depth < TREE_DEPTH_MAX && next.is_none_or(|level| level <= MODULES_DEPTH_MAX);
        if kind == "dir" && deeper {
            tree(&path, depth + 1, next, lines, seen);
        }
    }
}

/// Prints the last `TAIL_LINES` lines of the newest npm debug log in any of the folders.
/// Input: the folders that may hold npm's `_logs`.
fn npm_log(folders: &[PathBuf]) {
    let mut logs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for folder in folders {
        let Ok(read) = std::fs::read_dir(folder) else {
            println!("DIAG npm log folder absent {}", folder.display());
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let modified = entry.metadata().and_then(|meta| meta.modified());
            if let Ok(modified) = modified {
                println!("DIAG npm log found {}", path.display());
                logs.push((modified, path));
            }
        }
    }
    logs.sort();
    let Some((_, newest)) = logs.last() else {
        println!("DIAG npm log none");
        return;
    };
    let text = std::fs::read(newest).unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = text.lines().collect();
    println!(
        "DIAG npm log newest {} lines={}",
        newest.display(),
        lines.len()
    );
    for line in &lines[lines.len().saturating_sub(TAIL_LINES)..] {
        println!("DIAG npm log | {line}");
    }
}

/// Prints what the catalog lookup sees in the engine's shim folder: the search it builds, the
/// folder checks it makes and, for each name a Windows codex can have, the file, its real
/// path, the shim test and what the tool resolver makes of it. Inputs: the engine root and
/// the resolver's places.
fn lookup(root: &Path, places: &kernel_processes::finding::Places) {
    let prefix = crazyagent::setup::harness_prefix(root);
    let shims = kernel_processes::finding::npm_shims(&prefix);
    println!(
        "DIAG lookup search {:?}",
        Search::from_environment().with_engine(root)
    );
    println!(
        "DIAG lookup shim folder {} exists={} absolute={} bytes={} changeable_by_others={:?}",
        shims.display(),
        shims.is_dir(),
        shims.is_absolute(),
        shims.as_os_str().len(),
        kernel_processes::service::changeable_by_others(&shims)
    );
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    println!(
        "DIAG lookup TEMP {:?} TMP {:?}",
        std::env::var_os("TEMP"),
        std::env::var_os("TMP")
    );
    for name in SHIM_NAMES {
        let file = shims.join(name);
        if std::fs::symlink_metadata(&file).is_err() {
            println!("DIAG lookup {name} absent");
            continue;
        }
        println!(
            "DIAG lookup {name} file={} real={:?} is_shim={} resolve_entry={:?}",
            file.is_file(),
            std::fs::canonicalize(&file),
            harnesses::is_shim(home.as_deref(), &file),
            kernel_processes::finding::resolve_entry(&file, places).map(|resolved| resolved.argv())
        );
        if name == "codex.cmd" {
            let text = std::fs::read(&file).unwrap_or_default();
            for line in String::from_utf8_lossy(&text).lines().take(TAIL_LINES) {
                println!("DIAG lookup codex.cmd | {line}");
            }
        }
    }
    println!(
        "DIAG lookup catalog codex executable {:?}",
        codex(root).executable
    );
}

#[test]
#[ignore = "standard-user diagnostic: probe only"]
fn the_engine_npm_install_of_codex_as_the_gate_user_end_to_end() {
    println!("DIAG elevated {:?}", crazyagent_win_sandbox::elevated());
    let local = std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA for this user");
    let disk = tempfile::Builder::new()
        .prefix("ca-standard-user-diag-")
        .tempdir_in(&local)
        .unwrap();
    let root = kernel_sandbox::paths::canonical(disk.path()).unwrap();
    let db = crazyagent::landing::open(&root).unwrap();
    update::initialize(&db).unwrap();
    let roots = Roots::engine(&root);
    println!("DIAG root {}", root.display());
    println!("DIAG roots.home {}", roots.home.display());
    println!("DIAG roots.npm {:?}", roots.npm);
    println!(
        "DIAG harness_prefix {}",
        crazyagent::setup::harness_prefix(&root).display()
    );
    println!(
        "DIAG install_command {:?}",
        update::install_command("codex", &roots)
    );
    let before = codex(&root);
    println!("DIAG before executable {:?}", before.executable);

    let updates = Updates::new();
    let search_root = root.clone();
    let places = roots.places.clone();
    let (tell, told) = mpsc::channel();
    let signal = Ended(tell);
    let started = Instant::now();
    let installing = updates.install(&db, &before, &roots, move || {
        let _held = &signal;
        println!("DIAG after closure runs");
        lookup(&search_root, &places);
        codex(&search_root).executable
    });
    println!("DIAG install {installing:?}");
    let ended = installing.is_ok() && told.recv_timeout(JOB_TIME).is_ok();
    while ended && !updates.idle() && started.elapsed() < JOB_TIME {
        std::thread::yield_now();
    }
    println!(
        "DIAG job ended={ended} idle={} seconds={:.2}",
        updates.idle(),
        started.elapsed().as_secs_f64()
    );
    let last = updates.versions(&db, &before, &roots, false).last;
    match &last {
        Some(last) => {
            println!(
                "DIAG job last ok={} code={:?} from={:?} to={:?} ended_ms={}",
                last.ok, last.code, last.from, last.to, last.ended_ms
            );
            let lines: Vec<&str> = last.output.lines().collect();
            println!("DIAG job output lines={}", lines.len());
            for line in &lines[lines.len().saturating_sub(TAIL_LINES)..] {
                println!("DIAG job output | {line}");
            }
        }
        None => println!("DIAG job last none"),
    }

    let mut lines = 0;
    let mut seen = Vec::new();
    tree(&root, 0, None, &mut lines, &mut seen);
    let user = private::user_sid().map(|sid| sid.to_vec());
    match &user {
        Ok(user) => println!("DIAG gate user {}", sid_text(user)),
        Err(error) => println!("DIAG gate user unread: {error}"),
    }
    let user = user.unwrap_or_default();
    let prefix = crazyagent::setup::harness_prefix(&root);
    let mut folders: Vec<PathBuf> = seen
        .into_iter()
        .filter(|folder| {
            !folder.starts_with(prefix.join("node_modules"))
                || folder.components().count() <= prefix.components().count() + 3
        })
        .collect();
    folders.dedup();
    for folder in &folders {
        folder_access(folder, &user);
    }
    lookup(&root, &roots.places);
    let mut log_folders = vec![PathBuf::from(&local).join("npm-cache").join("_logs")];
    log_folders.push(
        roots
            .home
            .join("AppData")
            .join("Local")
            .join("npm-cache")
            .join("_logs"),
    );
    log_folders.dedup();
    npm_log(&log_folders);
    println!("DIAG done");
}
