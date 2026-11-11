use super::{ResearchKind, failure};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, Transaction, params};
use std::path::{Path, PathBuf};

#[cfg(all(test, unix))]
mod tests;

/// The most repository files one project links into its context.
pub const LINKS_MAX: usize = 32;
/// The most bytes of one linked path, below the project folder.
pub const LINK_PATH_BYTES_MAX: usize = 512;
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS project_doc_links (
    project TEXT NOT NULL,
    path TEXT NOT NULL,
    kind TEXT NOT NULL,
    PRIMARY KEY (project, path, kind)
);";
const ROADMAP: &str = "roadmap";
const RESEARCH_PREFIX: &str = "research:";

/// What a linked repository file stands for in the project's context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Roadmap,
    Research(ResearchKind),
}

/// One repository file linked into a project's context, by its path below the project folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocLink {
    pub path: String,
    pub kind: LinkKind,
}

/// Creates the links table. Input: the engine connection. Output: unit or `projects_unavailable`.
pub fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA)
        .map_err(|_| failure("projects_unavailable"))
}

/// Replaces a project's links. Inputs: the transaction, the project and at most [`LINKS_MAX`]
/// links, each a plain relative path below the project folder outside `.crazyagent`; a roadmap
/// planning file links as [`LinkKind::Roadmap`], never as research. Output: unit, or
/// `project_link_invalid` with nothing changed.
pub fn set(
    tx: &Transaction<'_>,
    project: &ProjectId,
    links: &[DocLink],
) -> Result<(), EngineError> {
    let valid = |link: &DocLink| {
        below_folder(&link.path) && link.kind != LinkKind::Research(ResearchKind::RoadmapPlanning)
    };
    if links.len() > LINKS_MAX || !links.iter().all(valid) {
        return Err(failure("project_link_invalid"));
    }
    tx.execute(
        "DELETE FROM project_doc_links WHERE project=?1",
        [project.as_str()],
    )
    .map_err(|_| failure("projects_unavailable"))?;
    for link in links {
        tx.execute(
            "INSERT OR IGNORE INTO project_doc_links(project,path,kind) VALUES (?1,?2,?3)",
            params![project.as_str(), link.path, kind_name(link.kind)],
        )
        .map_err(|_| failure("projects_unavailable"))?;
    }
    assert!(links.len() <= LINKS_MAX);
    Ok(())
}

/// Reads a project's links in path order, leaving out a stored row whose kind this engine
/// does not know or whose path is not a plain path below the project folder. Inputs: the
/// engine connection and the project. Output: at most [`LINKS_MAX`] links, or
/// `projects_unavailable`.
pub fn list(db: &Connection, project: &ProjectId) -> Result<Vec<DocLink>, EngineError> {
    let unavailable = |_| failure("projects_unavailable");
    let mut statement = db
        .prepare(
            "SELECT path, kind FROM project_doc_links WHERE project=?1
             ORDER BY path, kind LIMIT ?2",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(params![project.as_str(), LINKS_MAX as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(unavailable)?;
    let mut links = Vec::new();
    for row in rows {
        let (path, kind) = row.map_err(unavailable)?;
        if let Some(kind) = parse_kind(&kind).filter(|_| below_folder(&path)) {
            links.push(DocLink { path, kind });
        }
    }
    assert!(links.len() <= LINKS_MAX);
    Ok(links)
}

/// The file the project's roadmap is read from: the first linked roadmap that is a regular
/// file inside `folder`, else the roadmap the planning job wrote under the engine folder.
/// Inputs: the engine connection, the project and its folder. Output: the path, none when
/// neither exists, or `projects_unavailable`.
pub fn roadmap_file(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
) -> Result<Option<PathBuf>, EngineError> {
    let linked = list(db, project)?
        .into_iter()
        .filter(|link| link.kind == LinkKind::Roadmap)
        .find_map(|link| inside(folder, &link.path));
    let planned = crate::projects_home::state_folder(folder).join(crate::planning::ROADMAP);
    Ok(linked.or_else(|| regular(&planned).then_some(planned)))
}

/// The linked research files that are regular files inside `folder`, in path order.
/// Inputs: the engine connection, the project and its folder. Output: each file's research
/// kind and path, or `projects_unavailable`.
pub fn research_files(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
) -> Result<Vec<(ResearchKind, PathBuf)>, EngineError> {
    let files: Vec<(ResearchKind, PathBuf)> = list(db, project)?
        .into_iter()
        .filter_map(|link| match link.kind {
            LinkKind::Research(kind) => inside(folder, &link.path).map(|path| (kind, path)),
            LinkKind::Roadmap => None,
        })
        .collect();
    assert!(files.len() <= LINKS_MAX);
    Ok(files)
}

/// Whether `path` is a plain relative path below the project folder and outside the
/// engine's own folder: not empty, not absolute, no `.`, `..` or empty names, no control
/// characters, within [`LINK_PATH_BYTES_MAX`]. Input: the path. Output: the answer.
fn below_folder(path: &str) -> bool {
    let names: Vec<&str> = path.split('/').collect();
    !path.is_empty()
        && path.len() <= LINK_PATH_BYTES_MAX
        && !path.starts_with('/')
        && !path.chars().any(char::is_control)
        && names
            .iter()
            .all(|name| !name.is_empty() && *name != "." && *name != "..")
        && names[0] != crate::projects_home::CRAZYAGENT_DIRECTORY
}

/// The linked file joined to `folder` when it is a regular file, not a link, whose real path
/// stays inside the folder's real path, so a linked folder cannot lead out of the project.
/// Inputs: the project folder and the stored path. Output: the joined path, or none.
fn inside(folder: &Path, path: &str) -> Option<PathBuf> {
    assert!(below_folder(path));
    let joined = folder.join(path);
    let root = std::fs::canonicalize(folder).ok()?;
    let real = std::fs::canonicalize(&joined).ok()?;
    (regular(&joined) && real.starts_with(&root)).then_some(joined)
}

/// Whether `path` names a regular file itself rather than a link to one.
/// Input: the path. Output: the answer.
fn regular(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

/// The stored name of a link kind: `roadmap`, or `research:` and the research kind's name.
/// Input: the kind. Output: the name.
fn kind_name(kind: LinkKind) -> String {
    match kind {
        LinkKind::Roadmap => ROADMAP.to_owned(),
        LinkKind::Research(research) => format!("{RESEARCH_PREFIX}{}", research.name()),
    }
}

/// Reads a stored link kind name back. Input: the name. Output: the kind, or none for a name
/// this engine does not know.
fn parse_kind(name: &str) -> Option<LinkKind> {
    if name == ROADMAP {
        return Some(LinkKind::Roadmap);
    }
    let research = ResearchKind::parse(name.strip_prefix(RESEARCH_PREFIX)?)?;
    let kind = LinkKind::Research(research);
    assert_eq!(kind_name(kind), name);
    Some(kind)
}
