use rustix::fs::{Mode, OFlags};
use std::fs;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub const FILES_MAX: usize = 200;
pub const BYTES_MAX: u64 = 8 * 1024 * 1024;
pub const DEPTH_MAX: usize = 8;
pub const NAME_BYTES_MAX: usize = 64;
pub const DESCRIPTION_BYTES_MAX: usize = 1024;
pub const SOURCE_BYTES_MAX: usize = 1024;
const SKIPPED: [&str; 2] = [".git", ".DS_Store"];
const HEADER_LINES_MAX: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub files: usize,
    pub bytes: u64,
}

/// Reads the name and description from a SKILL.md file's frontmatter.
/// Input: the file text. Output: the validated name and one-line description,
/// or a plain rejection code when the frontmatter is missing or out of bounds.
pub fn frontmatter(text: &str) -> Result<(String, String), &'static str> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err("skill_frontmatter_missing");
    }
    let mut header: Vec<&str> = Vec::new();
    let mut closed = false;
    for line in lines.take(HEADER_LINES_MAX) {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        header.push(line);
    }
    if !closed {
        return Err("skill_frontmatter_missing");
    }
    let name = field(&header, "name").unwrap_or_default();
    let name_valid = !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if !name_valid {
        return Err("skill_name_invalid");
    }
    let description = field(&header, "description").unwrap_or_default();
    if description.is_empty()
        || description.len() > DESCRIPTION_BYTES_MAX
        || description.chars().any(char::is_control)
    {
        return Err("skill_description_invalid");
    }
    Ok((name, description))
}

fn field(header: &[&str], key: &str) -> Option<String> {
    let index = header.iter().position(|line| {
        line.strip_prefix(key)
            .is_some_and(|rest| rest.starts_with(':'))
    })?;
    let value = header[index][key.len() + 1..].trim();
    if matches!(value, ">" | "|" | ">-" | "|-" | ">+" | "|+") {
        let parts: Vec<&str> = header[index + 1..]
            .iter()
            .take_while(|line| line.starts_with([' ', '\t']) || line.trim().is_empty())
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect();
        return Some(parts.join(" "));
    }
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return Some(value[1..value.len() - 1].trim().to_owned());
        }
    }
    Some(value.to_owned())
}

struct Tally {
    files: usize,
    bytes: u64,
}

/// Copies an owner-picked skill folder into a new folder without running anything in it.
/// Inputs: the source folder and a destination path that must not exist yet.
/// Output: the skill's name, description and size, or a plain rejection code after
/// removing any partial copy.
pub fn copy(source: &Path, destination: &Path) -> Result<SkillMeta, &'static str> {
    if !source.is_absolute() || source.as_os_str().len() > SOURCE_BYTES_MAX {
        return Err("skill_folder_not_found");
    }
    let root = fs::canonicalize(source).map_err(|_| "skill_folder_not_found")?;
    if !fs::metadata(&root)
        .map_err(|_| "skill_folder_not_found")?
        .is_dir()
    {
        return Err("skill_not_a_folder");
    }
    if fs::symlink_metadata(destination).is_ok() {
        return Err("skill_store_unavailable");
    }
    fs::DirBuilder::new()
        .mode(0o700)
        .create(destination)
        .map_err(|_| "skill_store_unavailable")?;
    let mut tally = Tally { files: 0, bytes: 0 };
    let result = copy_tree(&root, &root, destination, 0, &mut tally).and_then(|()| {
        let path = destination.join("SKILL.md");
        if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            return Err("skill_missing_skill_md");
        }
        let text = fs::read_to_string(&path).map_err(|_| "skill_frontmatter_missing")?;
        let (name, description) = frontmatter(&text)?;
        Ok(SkillMeta {
            name,
            description,
            files: tally.files,
            bytes: tally.bytes,
        })
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    assert!(tally.files <= FILES_MAX && tally.bytes <= BYTES_MAX);
    result
}

fn copy_tree(
    root: &Path,
    from: &Path,
    to: &Path,
    depth: usize,
    tally: &mut Tally,
) -> Result<(), &'static str> {
    for entry in fs::read_dir(from).map_err(|_| "skill_unreadable")? {
        let entry = entry.map_err(|_| "skill_unreadable")?;
        let name = entry.file_name();
        if SKIPPED.iter().any(|skipped| name == *skipped) {
            continue;
        }
        let path = entry.path();
        let target = to.join(&name);
        let kind = fs::symlink_metadata(&path).map_err(|_| "skill_unreadable")?;
        if kind.is_dir() {
            if depth + 1 > DEPTH_MAX {
                return Err("skill_too_deep");
            }
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&target)
                .map_err(|_| "skill_store_unavailable")?;
            copy_tree(root, &path, &target, depth + 1, tally)?;
        } else if kind.is_file() {
            copy_file(&path, &target, tally)?;
        } else if kind.file_type().is_symlink() {
            let resolved = fs::canonicalize(&path).map_err(|_| "skill_link_outside")?;
            if !resolved.starts_with(root) {
                return Err("skill_link_outside");
            }
            if !fs::metadata(&resolved).is_ok_and(|m| m.is_file()) {
                return Err("skill_file_unsupported");
            }
            copy_file(&resolved, &target, tally)?;
        } else {
            return Err("skill_file_unsupported");
        }
    }
    Ok(())
}

fn copy_file(from: &Path, to: &Path, tally: &mut Tally) -> Result<(), &'static str> {
    let input = fs::File::from(
        rustix::fs::open(
            from,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|_| "skill_unreadable")?,
    );
    let metadata = input.metadata().map_err(|_| "skill_unreadable")?;
    if !metadata.is_file() {
        return Err("skill_file_unsupported");
    }
    if tally.files == FILES_MAX {
        return Err("skill_too_many_files");
    }
    tally.files += 1;
    let room = BYTES_MAX - tally.bytes;
    if metadata.len() > room {
        return Err("skill_too_large");
    }
    let mode = if metadata.permissions().mode() & 0o100 != 0 {
        0o700
    } else {
        0o600
    };
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(to)
        .map_err(|_| "skill_store_unavailable")?;
    let copied =
        std::io::copy(&mut input.take(room + 1), &mut output).map_err(|_| "skill_unreadable")?;
    if copied > room {
        return Err("skill_too_large");
    }
    if copied != metadata.len() {
        return Err("skill_unreadable");
    }
    tally.bytes += copied;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    const GOOD: &str = "---\nname: release-notes\ndescription: Writes release notes from merged changes.\n---\n\n# Release notes\n";

    fn skill(root: &Path, body: &str) -> PathBuf {
        let folder = root.join("source");
        fs::create_dir_all(folder.join("scripts")).unwrap();
        fs::write(folder.join("SKILL.md"), body).unwrap();
        fs::write(folder.join("scripts/run.sh"), "echo never run\n").unwrap();
        folder
    }

    #[test]
    fn frontmatter_needs_a_name_and_a_description() {
        assert_eq!(
            frontmatter(GOOD).unwrap(),
            (
                "release-notes".into(),
                "Writes release notes from merged changes.".into()
            )
        );
        assert_eq!(
            frontmatter("---\nname: \"tiger-style\"\ndescription: >\n  Safety first,\n  then speed.\nlicense: MIT\n---\nbody").unwrap(),
            ("tiger-style".into(), "Safety first, then speed.".into())
        );
        assert_eq!(
            frontmatter("\u{feff}---\r\nname: a1\r\ndescription: 'Quoted'\r\n---\r\n").unwrap(),
            ("a1".into(), "Quoted".into())
        );
        let cases = [
            ("# no frontmatter", "skill_frontmatter_missing"),
            ("---\nname: a\n", "skill_frontmatter_missing"),
            ("---\ndescription: d\n---\n", "skill_name_invalid"),
            (
                "---\nname: Bad Name\ndescription: d\n---\n",
                "skill_name_invalid",
            ),
            (
                "---\nname: -lead\ndescription: d\n---\n",
                "skill_name_invalid",
            ),
            ("---\nname: ok\n---\n", "skill_description_invalid"),
            (
                "---\nname: ok\ndescription: \"\"\n---\n",
                "skill_description_invalid",
            ),
        ];
        for (text, code) in cases {
            assert_eq!(frontmatter(text).unwrap_err(), code, "{text:?}");
        }
        let long = format!("---\nname: {}\ndescription: d\n---\n", "a".repeat(65));
        assert_eq!(frontmatter(&long).unwrap_err(), "skill_name_invalid");
        let wordy = format!("---\nname: a\ndescription: {}\n---\n", "d".repeat(1025));
        assert_eq!(
            frontmatter(&wordy).unwrap_err(),
            "skill_description_invalid"
        );
    }

    #[test]
    fn a_skill_folder_is_copied_whole_and_nothing_in_it_runs() {
        let root = tempfile::tempdir().unwrap();
        let source = skill(root.path(), GOOD);
        fs::write(source.join("notes.md"), "inside").unwrap();
        symlink("notes.md", source.join("alias.md")).unwrap();
        fs::create_dir(source.join(".git")).unwrap();
        fs::write(source.join(".git/HEAD"), "ref").unwrap();
        let destination = root.path().join("store/release-notes");
        fs::create_dir(root.path().join("store")).unwrap();
        let meta = copy(&source, &destination).unwrap();
        assert_eq!(meta.name, "release-notes");
        assert_eq!(
            meta.description,
            "Writes release notes from merged changes."
        );
        assert_eq!(meta.files, 4);
        assert_eq!(
            fs::read_to_string(destination.join("SKILL.md")).unwrap(),
            GOOD
        );
        assert_eq!(
            fs::read_to_string(destination.join("scripts/run.sh")).unwrap(),
            "echo never run\n"
        );
        let alias = fs::symlink_metadata(destination.join("alias.md")).unwrap();
        assert!(alias.file_type().is_file());
        assert_eq!(
            fs::read_to_string(destination.join("alias.md")).unwrap(),
            "inside"
        );
        assert!(!destination.join(".git").exists());
        assert_eq!(
            copy(&source, &destination).unwrap_err(),
            "skill_store_unavailable"
        );
    }

    #[test]
    fn unsafe_or_oversized_folders_are_refused_and_leave_nothing_behind() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("store")).unwrap();
        let outside = root.path().join("secret.txt");
        fs::write(&outside, "secret").unwrap();
        let destination = root.path().join("store/s");
        let empty = root.path().join("empty");
        fs::create_dir(&empty).unwrap();
        assert_eq!(
            copy(&empty, &destination).unwrap_err(),
            "skill_missing_skill_md"
        );
        assert_eq!(
            copy(&root.path().join("absent"), &destination).unwrap_err(),
            "skill_folder_not_found"
        );
        assert_eq!(
            copy(&outside, &destination).unwrap_err(),
            "skill_not_a_folder"
        );
        assert_eq!(
            copy(Path::new("relative"), &destination).unwrap_err(),
            "skill_folder_not_found"
        );

        let escaping = skill(&root.path().join("a"), GOOD);
        symlink(&outside, escaping.join("leak.txt")).unwrap();
        assert_eq!(
            copy(&escaping, &destination).unwrap_err(),
            "skill_link_outside"
        );

        let crowded = skill(&root.path().join("b"), GOOD);
        for index in 0..FILES_MAX {
            fs::write(crowded.join(format!("f{index}.md")), "x").unwrap();
        }
        assert_eq!(
            copy(&crowded, &destination).unwrap_err(),
            "skill_too_many_files"
        );

        let heavy = skill(&root.path().join("c"), GOOD);
        fs::write(heavy.join("big.bin"), vec![0u8; BYTES_MAX as usize + 1]).unwrap();
        assert_eq!(copy(&heavy, &destination).unwrap_err(), "skill_too_large");

        let deep = skill(&root.path().join("d"), GOOD);
        let mut nested = deep.clone();
        for level in 0..=DEPTH_MAX {
            nested = nested.join(format!("l{level}"));
        }
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("x.md"), "x").unwrap();
        assert_eq!(copy(&deep, &destination).unwrap_err(), "skill_too_deep");

        let unnamed = skill(&root.path().join("e"), "---\ndescription: d\n---\n");
        assert_eq!(
            copy(&unnamed, &destination).unwrap_err(),
            "skill_name_invalid"
        );

        let entries: Vec<_> = fs::read_dir(root.path().join("store")).unwrap().collect();
        assert!(entries.is_empty(), "partial copies must be removed");
    }
}
