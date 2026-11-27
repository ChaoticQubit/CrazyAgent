#![cfg(windows)]

use crazyagent::harnesses::update::Roots;
use std::path::PathBuf;

/// The folders of this process's PATH that hold Node, as written there, and the plain
/// installer folder. Output: the folders, each once.
fn node_folders() -> Vec<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut folders: Vec<PathBuf> = std::env::split_paths(&path)
        .filter(|folder| {
            folder
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("nodejs")
        })
        .collect();
    folders.push(PathBuf::from(r"C:\Program Files\nodejs"));
    folders.dedup();
    folders
}

#[test]
#[ignore = "standard-user diagnostic: probe only"]
fn the_node_folder_on_path_as_the_engine_reads_it() {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let before: Vec<PathBuf> = std::env::split_paths(&path).collect();
    println!("DIAG elevated {:?}", crazyagent_win_sandbox::elevated());
    println!("DIAG path folders before {} {before:?}", before.len());
    let local = std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA for this user");
    let disk = tempfile::Builder::new()
        .prefix("ca-diag-")
        .tempdir_in(local)
        .unwrap();
    let root = kernel_sandbox::paths::canonical(disk.path()).unwrap();
    let roots = Roots::engine(&root);
    let after = roots.places.folders();
    println!("DIAG places folders after {} {after:?}", after.len());
    println!("DIAG roots npm {:?}", roots.npm);
    for level in [
        PathBuf::from(r"C:\"),
        PathBuf::from(r"C:\Program Files"),
        PathBuf::from(r"C:\Program Files\nodejs"),
    ] {
        println!(
            "DIAG changeable_by_others {} -> {:?}",
            level.display(),
            kernel_processes::service::changeable_by_others(&level)
        );
    }
    for folder in node_folders() {
        println!(
            "DIAG folder {folder:?} kept {} agent_writable {}",
            after.contains(&folder),
            roots.places.agent_writable(&folder)
        );
        for name in ["node.exe", "npm.cmd"] {
            let file = folder.join(name);
            println!(
                "DIAG changeable_by_others {} -> {:?}",
                file.display(),
                kernel_processes::service::changeable_by_others(&file)
            );
            println!(
                "DIAG resolve_entry {} -> {:?}",
                file.display(),
                kernel_processes::finding::resolve_entry(&file, &roots.places)
            );
        }
    }
}
