use super::ProfileHooks;
use crate::paths::{any_case, at_or_below, regex_escape};
use crate::profile::consts::{
    ADMIN_LINKS, FIXED_WRITES, GIT_FALLBACK, MULTI_PACK_INDEX, SBPL_STRING_MAX, STORE_ALTERNATES,
};
use std::path::{Path, PathBuf};

/// Escapes a plain absolute path for a Seatbelt regex, so every character matches itself.
/// Input: a path that passes `clean_absolute`, so it holds no quote or backslash.
/// Output: the pattern text, without anchors.
pub fn seatbelt_pattern(path: &Path) -> String {
    let text = path.display().to_string();
    assert!(text.starts_with('/'));
    regex_escape(&text)
}

/// The Seatbelt rules that keep an agent from starting a macOS app: no exec of a main
/// executable in any app bundle's `Contents/MacOS`, wherever it sits and in any letter
/// case, since a GUI app started inside the agent profile aborts at launch and macOS shows
/// a crash report for every attempt (Chrome crash popups, decision C1, OP-24). The
/// interpreter a framework Python's launcher starts, `<name>.framework/Versions/<v>/
/// Resources/Python.app/Contents/MacOS/Python`, stays allowed, except in the folders the
/// agent can write, where it could plant a bundle under that name; their Data volume path
/// is denied too. Command-line tools a bundle ships under `Contents/Resources` or
/// `Contents/Developer` stay allowed, as does each exact program the engine launches from
/// outside those folders. It must follow the exec allow, since the last matching rule
/// wins. Inputs: the agent's writable roots and those programs. Output: the rule text.
pub fn app_exec_denies<'a>(
    writable: impl Iterator<Item = &'a PathBuf>,
    launched: impl Iterator<Item = &'a PathBuf>,
) -> String {
    let data = format!(
        "/{}/{}/{}",
        any_case("System"),
        any_case("Volumes"),
        any_case("Data")
    );
    let app = format!(
        ".+{}/{}/{}/",
        any_case(".app"),
        any_case("Contents"),
        any_case("MacOS")
    );
    let mut rule = format!(
        "(deny process-exec\n  (regex #\"^/{app}\"))\n(allow process-exec\n  (regex #\"^/.+\\.framework/Versions/[^/]+/Resources/Python\\.app/Contents/MacOS/Python$\"))\n(deny process-exec"
    );
    for root in writable {
        let root = root.display().to_string();
        rule.push_str(&format!(
            "\n  (regex #\"^({data})?{}/{app}\")",
            any_case(&root)
        ));
    }
    rule.push_str(")\n");
    let launched: Vec<&PathBuf> = launched.collect();
    if !launched.is_empty() {
        rule.push_str("(allow process-exec");
        for program in launched {
            rule.push_str(&format!("\n  (literal \"{}\")", program.display()));
        }
        rule.push_str(")\n");
    }
    rule
}

/// The Seatbelt rules that keep an Own branch agent from repointing the object store its
/// clone borrows (AL-1, DEC102-F1): no write to its `objects/info/alternates` or
/// `http-alternates`, its git folder's `commondir` or the git folder entry itself, which
/// it may never replace, rename or relink, no move or removal of its `objects` or
/// `objects/info` folder, no symlink in its store, and nothing at `.git.git` beside its
/// git folder, the fallback repository git's upload-pack serves when the git folder fails
/// git's repository test (RL-1), no `objects/pack/multi-pack-index` file, folder or
/// companion, which can name a pack outside the store (RL-2), and no write to the
/// `objects/pack` or `objects/<xx>` folder entries, so no folder built elsewhere is moved
/// in over one with its contents (RL-3), each name in any letter case, while its own loose
/// objects and packs inside those folders stay writable. The engine makes those folders
/// before each launch (`landing::ready_agent_clone`). The engine's fetch checks the same
/// store before it runs (w-landing). Input: the clone. Output: the rule text.
pub fn clone_store_denies(clone: &Path) -> String {
    let fallback = format!("{}/{}", seatbelt_pattern(clone), any_case(GIT_FALLBACK));
    let git = format!("{}/{}", seatbelt_pattern(clone), any_case(".git"));
    let objects = format!("{git}/{}", any_case("objects"));
    let info = format!("{objects}/{}", any_case("info"));
    let pack = format!("{objects}/{}", any_case("pack"));
    let index = format!("{pack}/{}", any_case(MULTI_PACK_INDEX));
    let [alternates, http_alternates] = STORE_ALTERNATES.map(any_case);
    let [commondir, _] = ADMIN_LINKS.map(any_case);
    let below = at_or_below(&fallback);
    let text = format!(
        "(deny file-write*\n  (regex #\"^{git}$\")\n  {below}\n  (regex #\"^{pack}$\")\n  (regex #\"^{objects}/[0-9a-fA-F][0-9a-fA-F]$\")\n  (regex #\"^{index}\")\n  (regex #\"^{info}/({alternates}|{http_alternates})$\")\n  (regex #\"^{git}/{commondir}$\"))\n(deny file-write-unlink\n  (regex #\"^{objects}$\")\n  (regex #\"^{info}$\"))\n(deny file-write-create\n  (require-all (regex #\"^{objects}/\") (vnode-type SYMLINK)))\n"
    );
    assert_eq!(text.matches("(regex #\"^/").count(), 10);
    assert!(text.contains("(vnode-type SYMLINK)"));
    text
}

/// The writes a clone lead's teammates may not make in the lead's clone (OP-9): anything but
/// the teammates' folder, the clone's object store, the inside of each worktree record git
/// keeps in `.git/worktrees`, and the refs and reflogs of the lead's own `<branch>--<name>`
/// teammate branches, none when the lead's branch is unknown, never the lead's branch
/// (AGENT-REF-ALLOW-NARROW); nor the links that tie a teammate worktree to the clone, which
/// the engine writes with the lead's rights, each name in any letter case; nor the
/// teammates' folder entry, nor make, move or remove anything directly in it, such as a
/// teammate's folder, where the engine's lead-rights `git worktree add` writes (OP9B-C1).
/// It must follow every write allow, since the last match wins.
/// Inputs: the profile hooks, the clone and the lead's branch. Output: the rule text.
pub fn teammates_clone_denies(hooks: &ProfileHooks, clone: &Path, lead: Option<&str>) -> String {
    let path = clone.display();
    let root = seatbelt_pattern(clone);
    let folder = hooks.teammates_folder;
    let teammates = any_case(folder);
    let git = any_case(".git");
    let branches = lead
        .map(|lead| {
            format!(
                "\n    (require-not (regex #\"^{root}/\\.git/(logs/)?refs/heads/{}--[a-z0-9-]+(\\.lock)?$\"))",
                regex_escape(lead)
            )
        })
        .unwrap_or_default();
    let text = format!(
        "(deny file-write*\n  (require-all\n    (subpath \"{path}\")\n    (require-not (subpath \"{path}/{folder}\"))\n    (require-not (subpath \"{path}/.git/objects\"))\n    (require-not (regex #\"^{root}/\\.git/worktrees/[^/]+/.+$\")){branches}))\n(deny file-write*\n  (regex #\"^{root}/{teammates}$\")\n  (regex #\"^{root}/{teammates}/[^/]+/{git}$\")\n  (regex #\"^{root}/{git}/{}/[^/]+/({}|{})$\"))\n(deny file-write-create file-write-unlink\n  (regex #\"^{root}/{teammates}/[^/]+$\"))\n",
        any_case("worktrees"),
        any_case("commondir"),
        any_case("gitdir")
    );
    assert_eq!(
        text.matches("(regex #\"^/").count(),
        5 + usize::from(lead.is_some())
    );
    assert_eq!(text.matches("(subpath \"/").count(), 3);
    text
}

/// Seatbelt regex bodies `{prefix}(a|b|...){suffix}` that together match each of
/// `alternatives`, packed whole, in order, so each body fits [`SBPL_STRING_MAX`] bytes; one
/// alternative too long beside the prefix keeps a body of its own, which [`sbpl_overlong`]
/// then names. Inputs: the prefix, the regex fragments and the suffix. Output: the bodies.
pub fn regex_bodies(prefix: &str, alternatives: &[String], suffix: &str) -> Vec<String> {
    let body = |group: &[&str]| format!("{prefix}({}){suffix}", group.join("|"));
    let mut bodies = Vec::new();
    let mut group: Vec<&str> = Vec::new();
    for alternative in alternatives {
        let mut wider = group.clone();
        wider.push(alternative);
        if !group.is_empty() && body(&wider).len() > SBPL_STRING_MAX {
            bodies.push(body(&group));
            group.clear();
        }
        group.push(alternative);
    }
    if !group.is_empty() {
        bodies.push(body(&group));
    }
    assert!(bodies.len() <= alternatives.len());
    bodies
}

/// The quoted strings and regexes of a Seatbelt profile longer than its reader takes
/// ([`SBPL_STRING_MAX`] bytes), every one, so a profile that would not parse is refused
/// before it is written. Input: the profile text, whose strings hold no quote. Output: the
/// strings too long, none for a profile that parses; an odd quote count names the rest.
#[cfg(unix)]
pub fn sbpl_overlong(profile: &str) -> Vec<&str> {
    let parts: Vec<&str> = profile.split('"').collect();
    let mut overlong: Vec<&str> = parts
        .iter()
        .skip(1)
        .step_by(2)
        .copied()
        .filter(|part| part.len() > SBPL_STRING_MAX)
        .collect();
    if parts.len().is_multiple_of(2) {
        overlong.push(parts[parts.len() - 1]);
    }
    overlong
}

/// Spells a Seatbelt regex for one path component other than `name` in any letter case,
/// with whatever follows it: a component that differs from `name` at some character, stops
/// short of it or runs past it. Input: a name of letters, digits and dots. Output: the
/// pattern text, a group without anchors.
pub fn other_than(name: &str) -> String {
    assert!(
        !name.is_empty()
            && name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '.')
    );
    let characters: Vec<char> = name.chars().collect();
    let mut alternatives = Vec::with_capacity(2 * characters.len());
    for at in 0..characters.len() {
        let prefix = any_case(&characters[..at].iter().collect::<String>());
        let character = characters[at];
        alternatives.push(format!(
            "{prefix}[^{}{}/]",
            character.to_ascii_lowercase(),
            character.to_ascii_uppercase()
        ));
        if at > 0 {
            alternatives.push(format!("{prefix}(/|$)"));
        }
    }
    alternatives.push(format!("{}[^/]", any_case(name)));
    assert_eq!(alternatives.len(), 2 * characters.len());
    format!("({})", alternatives.join("|"))
}

/// The Seatbelt rule that pins one writable root itself (CTX-M1): no agent may move, replace,
/// remove or re-permission a folder it writes in. A Seatbelt rename checks only the two
/// paths it names, so a root moved under another root and back would carry every path a
/// rule protects below it, such as the launch files in an agent's home (LL-2), out of that
/// rule and back. It must follow every write allow, since the last match wins. Input: the
/// root. Output: the rule text.
pub fn root_pin(root: &Path) -> String {
    format!("(deny {FIXED_WRITES} (literal \"{}\"))\n", root.display())
}

/// The Seatbelt filters for a `.git` at any depth below `root`, top level included, in every
/// spelling a case-insensitive volume finds ([`any_case`]): the part of [`ProjectSandbox::nested_git_denies`] for a root that is not a
/// working tree (SM1-R), as [`at_or_below`] spells it. Input: the root. Output: the filters.
pub fn any_depth_git(root: &Path) -> String {
    at_or_below(&format!(
        "{}/(.+/)?{}",
        seatbelt_pattern(root),
        any_case(".git")
    ))
}

/// The Seatbelt rule that refuses reading, writing, linking or cloning any path with a part
/// made of ASCII, [`crate::paths::ASCII_FOLDS`] points and at least one code
/// point HFS+ ignores in names, since such a volume opens `.g\u{200c}it/config` as
/// `.git/config` past every name deny (SA vid 202). A part in another script keeps its
/// joiners. Output: the rule, ending in a line break.
pub fn hfs_ignored_deny() -> String {
    let ignored: Vec<String> = crate::paths::HFS_IGNORED
        .iter()
        .map(char::to_string)
        .collect();
    let folds: Vec<String> = crate::paths::ASCII_FOLDS
        .iter()
        .map(|(point, _)| point.to_string())
        .collect();
    let ignored = ignored.join("|");
    let part = format!("([ -.0-~]|{}|{ignored})", folds.join("|"));
    let rule = format!(
        "(deny file-read-data file-write* file-link file-clone\n  (regex #\"^(.*/)?{part}*({ignored}){part}*(/.*)?$\"))\n"
    );
    assert!(rule.len() <= SBPL_STRING_MAX && rule.matches('"').count() == 2);
    rule
}

#[cfg(all(test, unix))]
mod tests;
