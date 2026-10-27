#[cfg(unix)]
use rustix::fs::{Mode, OFlags};
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const FILES_MAX: usize = 200;
pub const BYTES_MAX: u64 = 8 * 1024 * 1024;
pub const IMPORT_BYTES_MAX: u64 = 2 * 1024 * 1024;
pub const SKIPPED_LISTED_MAX: usize = 20;
pub const SKIPPED_PATH_BYTES_MAX: usize = 256;
pub const DEPTH_MAX: usize = 8;
pub const NAME_BYTES_MAX: usize = 64;
pub const DESCRIPTION_BYTES_MAX: usize = 1024;
pub const SOURCE_BYTES_MAX: usize = 1024;
#[cfg(unix)]
const SKIPPED: [&str; 2] = [".git", ".DS_Store"];
const HEADER_LINES_MAX: usize = 200;
#[cfg(unix)]
const ENTRIES_MAX: usize = 4096;
const PRIVATE_ENDINGS: [&str; 5] = [".pem", ".p12", ".pfx", ".keystore", ".jks"];
const PRIVATE_STARTS: [&str; 4] = ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];
const PRIVATE_WORDS: [&str; 5] = ["key", "secret", "credential", "password", "token"];

/// How a skill folder is copied: its total size limit with the code that refuses a larger
/// folder, and whether names that look private or like credentials are left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    pub bytes_max: u64,
    pub too_large: &'static str,
    pub skip_private: bool,
}

/// A folder the owner picks by hand: everything but `.git` and `.DS_Store`, up to 8 MiB.
pub const PICKED: Rules = Rules {
    bytes_max: BYTES_MAX,
    too_large: "skill_too_large",
    skip_private: false,
};

/// A skill brought in from the owner's own skills folders: up to 2 MiB, leaving out dotfiles
/// and anything that looks like a key, certificate or other credential.
pub const IMPORTED: Rules = Rules {
    bytes_max: IMPORT_BYTES_MAX,
    too_large: "skill_import_too_large",
    skip_private: true,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub files: usize,
    pub bytes: u64,
    pub skipped: Vec<String>,
    pub skipped_total: usize,
}

/// One file a copy under the same rules would make: its path inside the skill folder, the
/// file it is read from (a link's target inside the folder) and its size when checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub relative: PathBuf,
    pub from: PathBuf,
    pub bytes: u64,
}

/// Says whether a file or folder name looks private: a dotfile such as `.env`, a key or
/// certificate file, or a name holding key, secret, credential, password or token.
/// Input: one name, not a path. Output: true when an import leaves it out.
pub fn private(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_lowercase();
    name.starts_with('.')
        || PRIVATE_ENDINGS.iter().any(|ending| name.ends_with(ending))
        || PRIVATE_STARTS.iter().any(|start| name.starts_with(start))
        || PRIVATE_WORDS.iter().any(|word| name.contains(word))
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

#[cfg(unix)]
struct Tally {
    files: usize,
    bytes: u64,
    entries: usize,
    skipped: Vec<String>,
    skipped_total: usize,
    planned: Vec<PlannedFile>,
}

#[cfg(unix)]
impl Tally {
    fn skip(&mut self, relative: &Path) {
        self.skipped_total += 1;
        if self.skipped.len() < SKIPPED_LISTED_MAX {
            let shown: String = relative
                .to_string_lossy()
                .chars()
                .map(|c| if c.is_control() { '\u{fffd}' } else { c })
                .collect();
            let mut end = shown.len().min(SKIPPED_PATH_BYTES_MAX);
            while !shown.is_char_boundary(end) {
                end -= 1;
            }
            self.skipped.push(shown[..end].to_owned());
        }
    }
}

/// Copies an owner-picked skill folder into a new folder without running anything in it.
/// Inputs: the source folder and a destination path that must not exist yet.
/// Output: the skill's name, description and size, or a plain rejection code after
/// removing any partial copy.
#[cfg(unix)]
pub fn copy(source: &Path, destination: &Path) -> Result<SkillMeta, &'static str> {
    copy_with(source, destination, &PICKED)
}

/// Copies a skill folder under the given rules without running anything in it or changing
/// the source. Inputs: the source folder, a destination that must not exist yet and the
/// rules. Output: the skill's name, description, size and the paths left out (sorted, at
/// most `SKIPPED_LISTED_MAX`, with the total), or a rejection code after removing any copy.
#[cfg(unix)]
pub fn copy_with(
    source: &Path,
    destination: &Path,
    rules: &Rules,
) -> Result<SkillMeta, &'static str> {
    let (meta, planned) = walk(source, Some(destination), rules)?;
    assert!(planned.is_empty());
    Ok(meta)
}

/// Checks a skill folder in place under the given rules, reaching the verdict `copy_with`
/// would, and writes nothing. Inputs: the source folder and the rules. Output: the skill's
/// name, description, size and paths left out with each file a copy would make, or the
/// rejection code a copy would give.
#[cfg(unix)]
pub fn check_with(
    source: &Path,
    rules: &Rules,
) -> Result<(SkillMeta, Vec<PlannedFile>), &'static str> {
    walk(source, None, rules)
}

/// Reads one file a check planned without following a link, refusing it when it is no
/// longer a regular file of the checked size. Input: the planned file. Output: its bytes,
/// or a plain rejection code.
#[cfg(unix)]
pub fn read_planned(file: &PlannedFile) -> Result<Vec<u8>, &'static str> {
    let (input, metadata) = open(&file.from)?;
    if metadata.len() != file.bytes {
        return Err("skill_unreadable");
    }
    let mut bytes = Vec::new();
    input
        .take(file.bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "skill_unreadable")?;
    if bytes.len() as u64 != file.bytes {
        return Err("skill_unreadable");
    }
    Ok(bytes)
}

#[cfg(unix)]
fn walk(
    source: &Path,
    destination: Option<&Path>,
    rules: &Rules,
) -> Result<(SkillMeta, Vec<PlannedFile>), &'static str> {
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
    if let Some(destination) = destination {
        if fs::symlink_metadata(destination).is_ok() {
            return Err("skill_store_unavailable");
        }
        fs::DirBuilder::new()
            .mode(0o700)
            .create(destination)
            .map_err(|_| "skill_store_unavailable")?;
    }
    let mut tally = Tally {
        files: 0,
        bytes: 0,
        entries: 0,
        skipped: Vec::new(),
        skipped_total: 0,
        planned: Vec::new(),
    };
    let result = copy_tree(&root, &root, destination, 0, rules, &mut tally).and_then(|()| {
        let text = match destination {
            Some(destination) => {
                let path = destination.join("SKILL.md");
                if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                    return Err("skill_missing_skill_md");
                }
                fs::read_to_string(&path).map_err(|_| "skill_frontmatter_missing")?
            }
            None => {
                let file = tally
                    .planned
                    .iter()
                    .find(|file| file.relative == Path::new("SKILL.md"))
                    .ok_or("skill_missing_skill_md")?;
                String::from_utf8(read_planned(file)?).map_err(|_| "skill_frontmatter_missing")?
            }
        };
        let (name, description) = frontmatter(&text)?;
        let mut skipped = tally.skipped.clone();
        skipped.sort();
        Ok(SkillMeta {
            name,
            description,
            files: tally.files,
            bytes: tally.bytes,
            skipped,
            skipped_total: tally.skipped_total,
        })
    });
    if result.is_err()
        && let Some(destination) = destination
    {
        let _ = fs::remove_dir_all(destination);
    }
    assert!(tally.files <= FILES_MAX && tally.bytes <= rules.bytes_max);
    assert!(rules.skip_private || tally.skipped_total == 0);
    match destination {
        Some(_) => assert!(tally.planned.is_empty()),
        None => {
            assert!(tally.planned.iter().map(|file| file.bytes).sum::<u64>() == tally.bytes);
            assert!(
                tally.planned.len() == tally.files
                    || (result.is_err() && tally.planned.len() + 1 == tally.files)
            );
        }
    }
    result.map(|meta| (meta, tally.planned))
}

/// Refuses to copy a skill folder where its owner-only 0700 and 0600 copies cannot be
/// made. Takes the source and destination, both unused. Returns `skill_store_unavailable`.
#[cfg(not(unix))]
pub fn copy(_source: &Path, _destination: &Path) -> Result<SkillMeta, &'static str> {
    Err("skill_store_unavailable")
}

/// Refuses to copy a skill folder where its owner-only copies cannot be made. Takes the
/// source, destination and rules, all unused. Returns `skill_store_unavailable`.
#[cfg(not(unix))]
pub fn copy_with(
    _source: &Path,
    _destination: &Path,
    _rules: &Rules,
) -> Result<SkillMeta, &'static str> {
    Err("skill_store_unavailable")
}

/// Refuses to check a skill folder where its owner-only copies cannot be made, as
/// `copy_with` refuses. Takes the source and rules, both unused. Returns
/// `skill_store_unavailable`.
#[cfg(not(unix))]
pub fn check_with(
    _source: &Path,
    _rules: &Rules,
) -> Result<(SkillMeta, Vec<PlannedFile>), &'static str> {
    Err("skill_store_unavailable")
}

/// Refuses to read a planned file where no check can plan one. Takes the planned file,
/// unused. Returns `skill_store_unavailable`.
#[cfg(not(unix))]
pub fn read_planned(_file: &PlannedFile) -> Result<Vec<u8>, &'static str> {
    Err("skill_store_unavailable")
}

#[cfg(unix)]
fn copy_tree(
    root: &Path,
    from: &Path,
    to: Option<&Path>,
    depth: usize,
    rules: &Rules,
    tally: &mut Tally,
) -> Result<(), &'static str> {
    for entry in fs::read_dir(from).map_err(|_| "skill_unreadable")? {
        tally.entries += 1;
        if tally.entries > ENTRIES_MAX {
            return Err("skill_too_many_files");
        }
        let entry = entry.map_err(|_| "skill_unreadable")?;
        let name = entry.file_name();
        if SKIPPED.iter().any(|skipped| name == *skipped) {
            continue;
        }
        let path = entry.path();
        let relative = path.strip_prefix(root).map_err(|_| "skill_unreadable")?;
        if rules.skip_private && private(&name) {
            tally.skip(relative);
            continue;
        }
        let target = to.map(|to| to.join(&name));
        let kind = fs::symlink_metadata(&path).map_err(|_| "skill_unreadable")?;
        if kind.is_dir() {
            if depth + 1 > DEPTH_MAX {
                return Err("skill_too_deep");
            }
            if let Some(target) = &target {
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(target)
                    .map_err(|_| "skill_store_unavailable")?;
            }
            copy_tree(root, &path, target.as_deref(), depth + 1, rules, tally)?;
        } else if kind.is_file() {
            copy_file(&path, target.as_deref(), relative, rules, tally)?;
        } else if kind.file_type().is_symlink() {
            let resolved = fs::canonicalize(&path).map_err(|_| "skill_link_outside")?;
            if !resolved.starts_with(root) {
                return Err("skill_link_outside");
            }
            if !fs::metadata(&resolved).is_ok_and(|m| m.is_file()) {
                return Err("skill_file_unsupported");
            }
            let inside = resolved
                .strip_prefix(root)
                .map_err(|_| "skill_link_outside")?;
            if rules.skip_private && inside.iter().any(private) {
                tally.skip(relative);
                continue;
            }
            copy_file(&resolved, target.as_deref(), relative, rules, tally)?;
        } else {
            return Err("skill_file_unsupported");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn open(from: &Path) -> Result<(fs::File, fs::Metadata), &'static str> {
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
    Ok((input, metadata))
}

#[cfg(unix)]
fn copy_file(
    from: &Path,
    to: Option<&Path>,
    relative: &Path,
    rules: &Rules,
    tally: &mut Tally,
) -> Result<(), &'static str> {
    let (input, metadata) = open(from)?;
    if tally.files == FILES_MAX {
        return Err("skill_too_many_files");
    }
    tally.files += 1;
    let room = rules.bytes_max - tally.bytes;
    if metadata.len() > room {
        return Err(rules.too_large);
    }
    let Some(to) = to else {
        tally.planned.push(PlannedFile {
            relative: relative.to_path_buf(),
            from: from.to_path_buf(),
            bytes: metadata.len(),
        });
        tally.bytes += metadata.len();
        return Ok(());
    };
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
        return Err(rules.too_large);
    }
    if copied != metadata.len() {
        return Err("skill_unreadable");
    }
    tally.bytes += copied;
    Ok(())
}

#[cfg(all(test, unix))]
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

    fn snapshot(folder: &Path) -> Vec<(String, Vec<u8>)> {
        let mut found = Vec::new();
        let mut stack = vec![folder.to_path_buf()];
        while let Some(directory) = stack.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(folder).unwrap().display().to_string();
                let kind = fs::symlink_metadata(&path).unwrap();
                if kind.is_dir() {
                    stack.push(path);
                    found.push((relative, Vec::new()));
                } else if kind.file_type().is_symlink() {
                    let target = fs::read_link(&path).unwrap();
                    found.push((relative, target.display().to_string().into_bytes()));
                } else {
                    found.push((relative, fs::read(&path).unwrap()));
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn private_looking_names_are_the_ones_an_import_leaves_out() {
        for name in [
            ".env",
            ".netrc",
            "server.pem",
            "Cert.P12",
            "store.pfx",
            "app.keystore",
            "id_rsa",
            "id_ed25519.pub",
            "API_KEY.txt",
            "keys",
            "client_secret.json",
            "credentials.json",
            "password.txt",
            "github-token",
        ] {
            assert!(private(std::ffi::OsStr::new(name)), "{name}");
        }
        for name in ["SKILL.md", "notes.md", "scripts", "reference.pdf", "env.md"] {
            assert!(!private(std::ffi::OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn an_imported_folder_leaves_out_private_files_and_stays_within_2_mib() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("store")).unwrap();
        let source = skill(root.path(), GOOD);
        fs::write(source.join(".env"), "TOKEN=dummy").unwrap();
        fs::write(source.join("server.pem"), "pem").unwrap();
        fs::write(source.join("API_KEY.txt"), "k").unwrap();
        fs::create_dir(source.join("secrets")).unwrap();
        fs::write(source.join("secrets/a.md"), "s").unwrap();
        fs::write(source.join("notes.md"), "inside").unwrap();
        symlink(".env", source.join("env-link.md")).unwrap();
        symlink("notes.md", source.join("alias.md")).unwrap();
        let before = snapshot(&source);

        let destination = root.path().join("store/release-notes");
        let meta = copy_with(&source, &destination, &IMPORTED).unwrap();
        assert_eq!(
            meta.skipped,
            vec![
                ".env",
                "API_KEY.txt",
                "env-link.md",
                "secrets",
                "server.pem"
            ]
        );
        assert_eq!(meta.skipped_total, 5);
        assert_eq!(meta.files, 4);
        let copied: Vec<String> = snapshot(&destination)
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        assert_eq!(
            copied,
            vec![
                "SKILL.md",
                "alias.md",
                "notes.md",
                "scripts",
                "scripts/run.sh"
            ]
        );
        assert_eq!(
            snapshot(&source),
            before,
            "the source folder is never changed"
        );

        let picked = copy(&source, &root.path().join("store/picked")).unwrap();
        assert!(picked.skipped.is_empty());
        assert_eq!(picked.files, 9);

        let heavy = skill(&root.path().join("h"), GOOD);
        fs::write(
            heavy.join("big.bin"),
            vec![0u8; IMPORT_BYTES_MAX as usize + 1],
        )
        .unwrap();
        let refused = root.path().join("store/heavy");
        assert_eq!(
            copy_with(&heavy, &refused, &IMPORTED).unwrap_err(),
            "skill_import_too_large"
        );
        assert!(fs::symlink_metadata(&refused).is_err());
        copy(&heavy, &refused).unwrap();

        let escaping = skill(&root.path().join("x"), GOOD);
        fs::write(root.path().join("outside.md"), "secret").unwrap();
        symlink(root.path().join("outside.md"), escaping.join("leak.md")).unwrap();
        assert_eq!(
            copy_with(&escaping, &root.path().join("store/x"), &IMPORTED).unwrap_err(),
            "skill_link_outside"
        );

        let crowded = skill(&root.path().join("c"), GOOD);
        for index in 0..ENTRIES_MAX {
            fs::write(crowded.join(format!(".f{index}")), "x").unwrap();
        }
        assert_eq!(
            copy_with(&crowded, &root.path().join("store/c"), &IMPORTED).unwrap_err(),
            "skill_too_many_files"
        );
    }

    #[test]
    fn a_check_in_place_reaches_the_copys_verdict_and_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("store")).unwrap();
        let source = skill(root.path(), GOOD);
        fs::write(source.join(".env"), "TOKEN=dummy").unwrap();
        fs::write(source.join("notes.md"), "inside").unwrap();
        symlink("notes.md", source.join("alias.md")).unwrap();
        fs::create_dir(source.join(".git")).unwrap();
        fs::write(source.join(".git/HEAD"), "ref").unwrap();
        let heavy = skill(&root.path().join("h"), GOOD);
        fs::write(
            heavy.join("big.bin"),
            vec![0u8; IMPORT_BYTES_MAX as usize + 1],
        )
        .unwrap();
        let escaping = skill(&root.path().join("x"), GOOD);
        fs::write(root.path().join("outside.md"), "secret").unwrap();
        symlink(root.path().join("outside.md"), escaping.join("leak.md")).unwrap();
        let empty = root.path().join("empty");
        fs::create_dir(&empty).unwrap();
        let unnamed = skill(&root.path().join("e"), "---\ndescription: d\n---\n");
        let refusals = [
            (&heavy, "skill_import_too_large"),
            (&escaping, "skill_link_outside"),
            (&empty, "skill_missing_skill_md"),
            (&unnamed, "skill_name_invalid"),
        ];
        let before = snapshot(root.path());

        let (meta, planned) = check_with(&source, &IMPORTED).unwrap();
        for (folder, code) in refusals {
            assert_eq!(
                check_with(folder, &IMPORTED).unwrap_err(),
                code,
                "{folder:?}"
            );
        }
        assert_eq!(snapshot(root.path()), before, "a check writes nothing");

        let destination = root.path().join("store/release-notes");
        assert_eq!(copy_with(&source, &destination, &IMPORTED).unwrap(), meta);
        let mut checked: Vec<(String, Vec<u8>)> = planned
            .iter()
            .map(|file| {
                (
                    file.relative.display().to_string(),
                    read_planned(file).unwrap(),
                )
            })
            .collect();
        checked.sort();
        let copied: Vec<(String, Vec<u8>)> = snapshot(&destination)
            .into_iter()
            .filter(|(path, _)| destination.join(path).is_file())
            .collect();
        assert_eq!(checked, copied);
        for (folder, code) in refusals {
            assert_eq!(
                copy_with(folder, &root.path().join("store/refused"), &IMPORTED).unwrap_err(),
                code,
                "{folder:?}"
            );
        }
    }

    #[test]
    fn a_planned_file_that_changed_since_the_check_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let source = skill(root.path(), GOOD);
        let (_, planned) = check_with(&source, &IMPORTED).unwrap();
        let skill_md = planned
            .iter()
            .find(|file| file.relative == Path::new("SKILL.md"))
            .unwrap();
        assert_eq!(read_planned(skill_md).unwrap(), GOOD.as_bytes());
        fs::write(source.join("SKILL.md"), format!("{GOOD}More.\n")).unwrap();
        assert_eq!(read_planned(skill_md).unwrap_err(), "skill_unreadable");
        fs::remove_file(source.join("SKILL.md")).unwrap();
        symlink("scripts/run.sh", source.join("SKILL.md")).unwrap();
        assert_eq!(read_planned(skill_md).unwrap_err(), "skill_unreadable");
    }
}
