#![cfg(windows)]

use crazyagent::harnesses::update::{self, Roots};
use kernel_processes::finding::{Kind, resolve_entry};
use kernel_processes::install::{InstallPaths, InstallRecipe, Verb, steps};

const PACKAGE: &str = "@openai/codex";
const NODE_FLAGS: [&str; 3] = ["--no-warnings", "--no-deprecation", "--enable-source-maps"];

#[test]
#[ignore = "standard-user diagnostic: probe only"]
fn each_step_of_the_npm_install_command_as_the_engine_checks_it() {
    println!("DIAG elevated {:?}", crazyagent_win_sandbox::elevated());
    let local = std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA for this user");
    let disk = tempfile::Builder::new()
        .prefix("ca-standard-user-diag-")
        .tempdir_in(local)
        .unwrap();
    let root = kernel_sandbox::paths::canonical(disk.path()).unwrap();
    let roots = Roots::engine(&root);
    println!("DIAG roots.npm {:?}", roots.npm);
    println!(
        "DIAG install_command {:?}",
        update::install_command("codex", &roots)
    );
    let Some(npm) = roots.npm.clone() else {
        println!("DIAG no npm, nothing more to read");
        return;
    };
    println!("DIAG program file name {:?}", npm.program.file_name());
    let recipe = InstallRecipe::NpmGlobal {
        package: PACKAGE.to_owned(),
        version: String::new(),
        verb: Verb::Install,
        flags: Vec::new(),
    };
    let paths = InstallPaths {
        program: Some(npm.program.clone()),
        npm_prefix: Some(npm.prefix.clone()),
        ..InstallPaths::default()
    };
    println!(
        "DIAG step matched steps(npm.program) {:?}",
        steps(&recipe, &paths)
    );
    let named = InstallPaths {
        program: Some(npm.program.with_file_name("npm")),
        ..paths.clone()
    };
    println!(
        "DIAG step matched control steps(<folder>\\npm) {:?}",
        steps(&recipe, &named)
    );
    println!(
        "DIAG step trusted program_trusted {:?}",
        crazyagent_win_sandbox::program_trusted(&npm.program)
    );
    for folder in &npm.search {
        println!(
            "DIAG step vetted search {} exists={} kept_for_children={}",
            folder.display(),
            std::fs::symlink_metadata(folder).is_ok(),
            roots.places.kept_for_children(folder)
        );
    }
    let resolved = resolve_entry(&npm.program, &roots.places);
    println!("DIAG step vetted resolve_entry {resolved:?}");
    if let Ok(resolved) = resolved {
        let mut at = resolved;
        for level in 0..4 {
            let Kind::Script {
                interpreter,
                arguments,
                by_name,
            } = at.kind.clone()
            else {
                println!(
                    "DIAG step vetted level {level} Native {}",
                    at.program.display()
                );
                break;
            };
            let name = interpreter.program.file_name().map(|name| name.to_owned());
            let flags_only = arguments
                .iter()
                .all(|word| NODE_FLAGS.contains(&word.as_str()));
            println!(
                "DIAG step vetted level {level} interpreter {name:?} arguments {arguments:?} by_name {by_name} node_flags_only {flags_only}"
            );
            at = *interpreter;
        }
    }
    println!(
        "DIAG step link_hops link_hops_kept {}",
        roots.places.link_hops_kept(&npm.program)
    );
}
