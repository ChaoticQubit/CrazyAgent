use std::path::PathBuf;

const NODE: &str = if cfg!(windows) { "node.exe" } else { "node" };
const NPM: &str = if cfg!(windows) { "npm.cmd" } else { "npm" };

/// A Node the tool resolver finds whole in one folder: the `node` entry and the `npm` entry
/// beside it, whose own interpreter resolves too, as found on the places before any link is
/// followed, so a link to `node` keeps following the owner's own link, such as Homebrew's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usable {
    pub node: PathBuf,
    pub npm: PathBuf,
}

/// The one test of whether this machine has Node, for setup and npm harness installs: the
/// first folder on the places where the tool resolver accepts both `node` and `npm` (`node.exe`
/// and `npm.cmd` on Windows, npm with its interpreter), so npm never pairs with another
/// install's node; the engine's own is found through its `setup/bin` links. Nothing is run.
/// Input: the places. Output: the Node, or none when no folder holds both.
pub fn usable(places: &kernel_processes::finding::Places) -> Option<Usable> {
    let found = places.folders().iter().find_map(|folder| {
        let node = kernel_processes::finding::resolve_entry(&folder.join(NODE), places).ok()?;
        let npm = kernel_processes::finding::resolve_entry(&folder.join(NPM), places).ok()?;
        Some((node, npm))
    });
    let Some((node, npm)) = found else {
        kernel_processes::finding::probe::missed(crate::tools::finding_seed);
        return None;
    };
    assert_eq!(node.entry.parent(), npm.entry.parent());
    assert!(node.entry.is_absolute() && node.program.is_absolute());
    Some(Usable {
        node: node.entry,
        npm: npm.entry,
    })
}
