use crate::client_controller::OwnerController;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

pub use crate::plugins::SkillView as SkillInfo;

pub const LINE_BYTES_MAX: usize = 160;
pub const TEXT_BYTES_MAX: usize = 1024 * 1024;
pub const PAGE_TEXT_BYTES_MAX: usize = 24 * 1024;
pub const FILES_LISTED_MAX: usize = crate::plugins::skill_folder::FILES_MAX;
pub const FILES_PAGE_BYTES_MAX: usize = 16 * 1024;
pub const PATH_BYTES_MAX: usize = 512;
const ENTRIES_MAX: usize = 4096;
const FILE_ENTRY_BYTES: usize = 32;
const ELLIPSIS: &str = "...";

/// One file in a skill folder as the detail view lists it: its path inside the folder with
/// `/` between parts, control characters shown as U+FFFD, and its size in bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillFile {
    pub path: String,
    pub bytes: u64,
}

/// One engine reply of a skill's detail, small enough for one frame: the skill, the part
/// of its SKILL.md text that starts at the asked byte offset, the offset of the next part,
/// and on the first page the folder's files with how many more were left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPage {
    pub skill: SkillInfo,
    pub text: String,
    pub next: Option<u32>,
    pub files: Vec<SkillFile>,
    pub files_more: u32,
}

/// A skill's whole detail for its detail view: the skill with its switches, the full
/// SKILL.md text, the folder's files and how many more files were left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillDetail {
    pub skill: SkillInfo,
    pub text: String,
    pub files: Vec<SkillFile>,
    pub files_more: u32,
}

fn error(code: &str) -> EngineError {
    EngineError(code.into())
}

fn clip(text: &str, bytes: usize) -> &str {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Shortens a skill's description to the one line a tag hover or a list row shows: its
/// first sentence, cut before a word with "..." when it is longer than `LINE_BYTES_MAX`.
/// Input: the description from SKILL.md. Output: at most `LINE_BYTES_MAX` bytes, empty
/// only for an empty description.
pub fn one_line(description: &str) -> String {
    let text = description.trim();
    let sentence = match text.find(". ") {
        Some(end) => &text[..=end],
        None => text,
    };
    if sentence.len() <= LINE_BYTES_MAX {
        return sentence.to_owned();
    }
    let room = clip(sentence, LINE_BYTES_MAX - ELLIPSIS.len());
    let cut = match room.rfind(char::is_whitespace) {
        Some(space) if space > 0 => room[..space].trim_end(),
        _ => room,
    };
    let line = format!("{cut}{ELLIPSIS}");
    assert!(line.len() <= LINE_BYTES_MAX);
    line
}

/// Lists the skill catalog the app shows and tags: every skill in global settings, or for
/// a project its own skills plus the global ones with that project's switch. Inputs: a
/// connected owner controller and the optional project. Output: each skill's id, name,
/// one-line description, source folder, app switch (`enabled`) and project switch
/// (`on_here`), in the engine's order.
pub async fn catalog(
    controller: &OwnerController,
    project: Option<ProjectId>,
) -> Result<Vec<SkillInfo>, EngineError> {
    controller.plugin_skills(project).await
}

fn escaped(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        other if u32::from(other) < 0x20 => 6,
        other => other.len_utf8(),
    }
}

fn read_text(path: &Path) -> Result<String, EngineError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| error("skill_not_found"))?;
    if !metadata.is_file() {
        return Err(error("skill_unreadable"));
    }
    if metadata.len() > TEXT_BYTES_MAX as u64 {
        return Err(error("skill_text_too_large"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| error("skill_unreadable"))?
        .take(TEXT_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error("skill_unreadable"))?;
    if bytes.len() > TEXT_BYTES_MAX {
        return Err(error("skill_text_too_large"));
    }
    String::from_utf8(bytes).map_err(|_| error("skill_unreadable"))
}

fn shown(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect()
}

fn list(folder: &Path) -> Result<(Vec<SkillFile>, u32), EngineError> {
    let unreadable = |_| error("skill_unreadable");
    let mut found = Vec::new();
    let mut visited = 0usize;
    let mut stack: Vec<(PathBuf, String, usize)> = vec![(folder.to_path_buf(), String::new(), 0)];
    while let Some((directory, prefix, depth)) = stack.pop() {
        for entry in std::fs::read_dir(&directory).map_err(unreadable)? {
            visited += 1;
            if visited > ENTRIES_MAX {
                return Err(error("skill_too_many_files"));
            }
            let entry = entry.map_err(unreadable)?;
            let name = shown(&entry.file_name().to_string_lossy());
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type().map_err(unreadable)?;
            if kind.is_dir() && depth < crate::plugins::skill_folder::DEPTH_MAX {
                stack.push((entry.path(), path, depth + 1));
            } else if kind.is_file() {
                let bytes = entry.metadata().map_err(unreadable)?.len();
                let path = clip(&path, PATH_BYTES_MAX).to_owned();
                found.push(SkillFile { path, bytes });
            }
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    let mut cost = 0;
    let mut kept = 0;
    for file in &found {
        let size = FILE_ENTRY_BYTES + file.path.chars().map(escaped).sum::<usize>();
        if kept == FILES_LISTED_MAX || cost + size > FILES_PAGE_BYTES_MAX {
            break;
        }
        cost += size;
        kept += 1;
    }
    let more = u32::try_from(found.len() - kept).map_err(|_| error("skill_too_many_files"))?;
    found.truncate(kept);
    assert!(found.len() <= FILES_LISTED_MAX && cost <= FILES_PAGE_BYTES_MAX);
    Ok((found, more))
}

/// Reads one page of a skill's detail from the engine's copy of its folder, never
/// following a link to SKILL.md. Inputs: the skill as the engine lists it and the byte
/// offset into SKILL.md, 0 for the first page. Output: the page whose text, escaped for a
/// reply, stays within `PAGE_TEXT_BYTES_MAX`, with the file list on the first page only;
/// or `skill_page_invalid` for an offset that is not where a page ends,
/// `skill_text_too_large` above `TEXT_BYTES_MAX`, or `skill_unreadable`.
pub fn page(skill: SkillInfo, offset: u32) -> Result<SkillPage, EngineError> {
    let folder = PathBuf::from(&skill.folder);
    let text = read_text(&folder.join("SKILL.md"))?;
    let start = usize::try_from(offset).map_err(|_| error("skill_page_invalid"))?;
    if start > text.len() || !text.is_char_boundary(start) {
        return Err(error("skill_page_invalid"));
    }
    let mut end = start;
    let mut cost = 0;
    for character in text[start..].chars() {
        let size = escaped(character);
        if cost + size > PAGE_TEXT_BYTES_MAX {
            break;
        }
        cost += size;
        end += character.len_utf8();
    }
    assert!(end > start || start == text.len());
    let next = if end < text.len() {
        Some(u32::try_from(end).map_err(|_| error("skill_text_too_large"))?)
    } else {
        None
    };
    let (files, files_more) = if offset == 0 {
        list(&folder)?
    } else {
        (Vec::new(), 0)
    };
    Ok(SkillPage {
        skill,
        text: text[start..end].to_owned(),
        next,
        files,
        files_more,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(folder: &Path) -> SkillInfo {
        SkillInfo {
            id: "skill-1".into(),
            name: "notes".into(),
            description: "Writes notes.".into(),
            line: "Writes notes.".into(),
            scope: crate::plugins::Scope::Global,
            scope_label: "For every project".into(),
            enabled: true,
            on_here: None,
            source: "/picked/notes".into(),
            folder: folder.display().to_string(),
        }
    }

    #[test]
    fn the_line_is_the_first_sentence_cut_before_a_word() {
        assert_eq!(
            one_line("Persistent knowledge graph with query and path. Answer any question."),
            "Persistent knowledge graph with query and path."
        );
        assert_eq!(
            one_line("  Builds polished front ends  "),
            "Builds polished front ends"
        );
        assert_eq!(
            one_line("Version 1.2 ships. Then more."),
            "Version 1.2 ships."
        );
        assert_eq!(one_line(""), "");
        let long = format!("{} tail", "word ".repeat(60));
        let line = one_line(&long);
        assert!(line.len() <= LINE_BYTES_MAX, "{}", line.len());
        assert!(line.ends_with("word..."), "{line}");
        let unbroken = "x".repeat(400);
        let line = one_line(&unbroken);
        assert_eq!(line.len(), LINE_BYTES_MAX);
        assert!(line.ends_with("..."));
        let wide = "é".repeat(200);
        let line = one_line(&wide);
        assert!(line.len() <= LINE_BYTES_MAX && line.ends_with("..."));
        assert!(line.trim_end_matches("...").chars().all(|c| c == 'é'));
    }

    #[test]
    fn pages_join_into_the_whole_skill_md_and_list_its_files_once() {
        let folder = tempfile::tempdir().unwrap();
        let text = format!(
            "---\nname: notes\ndescription: Writes notes.\n---\n{}",
            "line with \"quotes\" and \\ and é\n".repeat(3000)
        );
        std::fs::write(folder.path().join("SKILL.md"), &text).unwrap();
        std::fs::create_dir_all(folder.path().join("references/deep")).unwrap();
        std::fs::write(folder.path().join("references/a.md"), "aaaa").unwrap();
        std::fs::write(folder.path().join("references/deep/b.txt"), "b").unwrap();
        let mut joined = String::new();
        let mut offset = 0;
        let mut pages = 0;
        loop {
            let page = page(info(folder.path()), offset).unwrap();
            let reply = serde_json::to_vec(&page).unwrap();
            assert!(
                reply.len() <= crate::protocol::FRAME_BYTES_MAX,
                "{}",
                reply.len()
            );
            assert_eq!(page.files.is_empty(), offset != 0);
            if offset == 0 {
                let paths: Vec<(&str, u64)> = page
                    .files
                    .iter()
                    .map(|file| (file.path.as_str(), file.bytes))
                    .collect();
                assert_eq!(
                    paths,
                    vec![
                        ("SKILL.md", text.len() as u64),
                        ("references/a.md", 4),
                        ("references/deep/b.txt", 1),
                    ]
                );
                assert_eq!(page.files_more, 0);
            }
            joined.push_str(&page.text);
            pages += 1;
            match page.next {
                Some(next) => offset = next,
                None => break,
            }
        }
        assert_eq!(joined, text);
        assert!(pages > 3, "{pages}");
        assert_eq!(
            page(info(folder.path()), 1_000_000).unwrap_err().0,
            "skill_page_invalid"
        );
        let wide = "é".repeat(10);
        std::fs::write(folder.path().join("SKILL.md"), &wide).unwrap();
        assert_eq!(
            page(info(folder.path()), 1).unwrap_err().0,
            "skill_page_invalid"
        );
    }

    #[test]
    fn a_large_linked_or_odd_skill_md_is_refused_and_odd_file_names_are_shown_safely() {
        let folder = tempfile::tempdir().unwrap();
        let skill = folder.path().join("SKILL.md");
        std::fs::write(&skill, vec![b'a'; TEXT_BYTES_MAX + 1]).unwrap();
        assert_eq!(
            page(info(folder.path()), 0).unwrap_err().0,
            "skill_text_too_large"
        );
        std::fs::write(&skill, [0xff, 0xfe]).unwrap();
        assert_eq!(
            page(info(folder.path()), 0).unwrap_err().0,
            "skill_unreadable"
        );
        std::fs::remove_file(&skill).unwrap();
        assert_eq!(
            page(info(folder.path()), 0).unwrap_err().0,
            "skill_not_found"
        );
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("secret.md"), "secret").unwrap();
            std::os::unix::fs::symlink(outside.path().join("secret.md"), &skill).unwrap();
            assert_eq!(
                page(info(folder.path()), 0).unwrap_err().0,
                "skill_unreadable"
            );
            std::fs::remove_file(&skill).unwrap();
            std::fs::write(&skill, "Body").unwrap();
            std::fs::write(folder.path().join("bad\u{1b}[2Jname"), "x").unwrap();
            std::os::unix::fs::symlink(outside.path(), folder.path().join("link")).unwrap();
            let listed = page(info(folder.path()), 0).unwrap();
            let paths: Vec<&str> = listed.files.iter().map(|file| file.path.as_str()).collect();
            assert_eq!(paths, vec!["SKILL.md", "bad\u{fffd}[2Jname"]);
        }
    }

    #[test]
    fn a_folder_with_more_files_than_one_page_counts_the_rest() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("SKILL.md"), "Body").unwrap();
        let long = "n".repeat(200);
        for index in 0..150 {
            std::fs::write(folder.path().join(format!("{index:03}{long}")), "x").unwrap();
        }
        let listed = page(info(folder.path()), 0).unwrap();
        assert!(listed.files.len() < 151);
        assert_eq!(listed.files.len() + listed.files_more as usize, 151);
        assert!(serde_json::to_vec(&listed).unwrap().len() <= crate::protocol::FRAME_BYTES_MAX);
    }
}
