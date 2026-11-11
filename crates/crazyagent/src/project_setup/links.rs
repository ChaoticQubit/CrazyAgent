use super::{ResearchKind, failure};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, Transaction, params};
use std::io::Read;
use std::path::Path;

#[cfg(all(test, unix))]
mod tests;

/// The most repository files one project links into its context.
pub const LINKS_MAX: usize = 32;
/// The most bytes of one linked path, below the project folder.
pub const LINK_PATH_BYTES_MAX: usize = 512;
/// The most bytes read from one linked file; a longer file is not read at all.
pub const LINK_BYTES_MAX: u64 = 256 * 1024;
/// The most stored rows one listing looks at, so rows of kinds a newer engine wrote
/// cannot hide the links this engine reads.
const LINK_ROWS_SCANNED: usize = 8 * LINKS_MAX;
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

/// The text of one file read below the project folder, with its path below that folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedText {
    pub path: String,
    pub text: String,
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
        .query_map(params![project.as_str(), LINK_ROWS_SCANNED as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(unavailable)?;
    let mut links = Vec::new();
    for row in rows {
        let (path, kind) = row.map_err(unavailable)?;
        if let Some(kind) = parse_kind(&kind).filter(|_| below_folder(&path)) {
            links.push(DocLink { path, kind });
        }
        if links.len() == LINKS_MAX {
            break;
        }
    }
    Ok(links)
}

/// The project's roadmap: the first linked roadmap [`read_linked`] reads, else the roadmap
/// the planning job wrote in the engine folder, read the same way. Inputs: the engine
/// connection, the project and its folder. Output: the roadmap, none when neither reads, or
/// `projects_unavailable`.
pub fn roadmap(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
) -> Result<Option<LinkedText>, EngineError> {
    let linked = list(db, project)?
        .into_iter()
        .filter(|link| link.kind == LinkKind::Roadmap)
        .find_map(|link| {
            read_linked(folder, &link.path).map(|text| LinkedText {
                path: link.path,
                text,
            })
        });
    if linked.is_some() {
        return Ok(linked);
    }
    let planned = if crate::projects_home::state_folder(folder) == folder {
        crate::planning::ROADMAP.to_owned()
    } else {
        format!(
            "{}/{}",
            crate::projects_home::CRAZYAGENT_DIRECTORY,
            crate::planning::ROADMAP
        )
    };
    Ok(
        read_beneath(folder, Path::new(&planned)).map(|text| LinkedText {
            path: planned,
            text,
        }),
    )
}

/// The linked research files [`read_linked`] reads, in path order. Inputs: the engine
/// connection, the project and its folder. Output: each file's research kind and text, or
/// `projects_unavailable`.
pub fn research(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
) -> Result<Vec<(ResearchKind, LinkedText)>, EngineError> {
    Ok(list(db, project)?
        .into_iter()
        .filter_map(|link| match link.kind {
            LinkKind::Research(kind) => read_linked(folder, &link.path).map(|text| {
                (
                    kind,
                    LinkedText {
                        path: link.path,
                        text,
                    },
                )
            }),
            LinkKind::Roadmap => None,
        })
        .collect())
}

/// Reads one linked file below `folder` as text, opened with no link followed at any part
/// of its path and no FIFO waited on, then checked through the open file itself, so nothing
/// swapped in after the link was saved is read. Inputs: the project folder and the stored
/// path. Output: the text, or none for a path [`set`] refuses, a missing file, a link, a
/// folder, a FIFO, a hard-linked file, more than [`LINK_BYTES_MAX`] bytes or invalid UTF-8.
pub fn read_linked(folder: &Path, path: &str) -> Option<String> {
    if !below_folder(path) {
        return None;
    }
    read_beneath(folder, Path::new(path))
}

/// Reads one regular file below `folder` through [`crate::platform::open_plain_beneath`],
/// refusing a file with a second hard link, since its other name may lie outside the folder.
/// Inputs: the folder and the relative path. Output: the text, or none.
fn read_beneath(folder: &Path, below: &Path) -> Option<String> {
    let file = crate::platform::open_plain_beneath(folder, below).ok()?;
    let opened = file.metadata().ok()?;
    #[cfg(unix)]
    let single = std::os::unix::fs::MetadataExt::nlink(&opened) == 1;
    #[cfg(not(unix))]
    let single = true;
    if !opened.is_file() || !single || opened.len() > LINK_BYTES_MAX {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(LINK_BYTES_MAX + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > LINK_BYTES_MAX {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Whether `path` is a plain relative path below the project folder and outside the
/// engine's own folder in any letter case: not empty, not absolute, no `.`, `..` or empty
/// names, no control characters, `\` or `:`, within [`LINK_PATH_BYTES_MAX`]. Input: the
/// path. Output: the answer.
pub(crate) fn below_folder(path: &str) -> bool {
    let names: Vec<&str> = path.split('/').collect();
    !path.is_empty()
        && path.len() <= LINK_PATH_BYTES_MAX
        && !path.starts_with('/')
        && !path
            .chars()
            .any(|letter| letter.is_control() || letter == '\\' || letter == ':')
        && names
            .iter()
            .all(|name| !name.is_empty() && *name != "." && *name != "..")
        && !names[0].eq_ignore_ascii_case(crate::projects_home::CRAZYAGENT_DIRECTORY)
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
