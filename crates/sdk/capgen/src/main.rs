#![forbid(unsafe_code)]

use std::path::Path;
use std::process::ExitCode;

/// Runs capgen: `--write <caps dir> <generated dir>` writes the Rust SDK files there,
/// `--check <caps dir> <generated dir>` compares them. Input: the command line.
/// Output: 0 when written or equal, 1 with each differing path on drift, 2 on a refusal.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [mode, caps, generated] = args.as_slice() else {
        eprintln!("usage: capgen --write|--check <caps dir> <generated dir>");
        return ExitCode::from(2);
    };
    let generated = Path::new(generated);
    let files = match crazyagent_capgen::load(Path::new(caps))
        .and_then(|loaded| {
            let borrowed: Vec<(&str, crazyagent_cap::File)> = loaded
                .iter()
                .map(|(stem, file)| (stem.as_str(), file.clone()))
                .collect();
            crazyagent_capgen::rust(&borrowed)
        }) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("{} {}: {}", error.code, error.file, error.detail);
            return ExitCode::from(2);
        }
    };
    match mode.as_str() {
        "--write" => {
            if let Err(error) = std::fs::create_dir_all(generated) {
                eprintln!("capgen_write {}: {error}", generated.display());
                return ExitCode::from(2);
            }
            for path in crazyagent_capgen::drift(generated, &files) {
                if !files.iter().any(|(name, _)| *name == path) {
                    eprintln!("capgen_extra_file {path}");
                    return ExitCode::from(2);
                }
            }
            for (path, text) in &files {
                if let Err(error) = std::fs::write(generated.join(path), text) {
                    eprintln!("capgen_write {path}: {error}");
                    return ExitCode::from(2);
                }
            }
            ExitCode::SUCCESS
        }
        "--check" => {
            let differing = crazyagent_capgen::drift(generated, &files);
            for path in &differing {
                println!("{path}");
            }
            if differing.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        _ => {
            eprintln!("usage: capgen --write|--check <caps dir> <generated dir>");
            ExitCode::from(2)
        }
    }
}
