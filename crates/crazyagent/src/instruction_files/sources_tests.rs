use super::{Source, read, read_between, sources};
use crate::harnesses::registry::Registry;
use crate::tools::Places;
use std::fs;
use std::path::{Path, PathBuf};

/// A harness manifest that keeps its files in one owner folder. Inputs: the id, the shown
/// name, the owner folder and the import block as YAML, empty for none. Output: the text.
fn listed(id: &str, name: &str, owner: &str, import: &str) -> String {
    format!(
        "schema: 1\nkind: harness\nid: \"{id}\"\nname: \"{name}\"\nbinaries:\n  - \"{id}\"\nheadless: \"{id} --json\"\nmodel_selection: true\naccount_kinds:\n  - \"api_key\"\nhome:\n  owner_folders:\n    - \"{owner}\"\n{import}"
    )
}

/// The registry of these rows, living as long as the process: two harnesses that name an
/// instruction file that is there, one whose file is missing, one whose file is a folder,
/// one that names only skills and one with no import block. Output: the registry.
fn registry() -> &'static Registry {
    let files = [
        (
            "standin-pi",
            listed(
                "standin-pi",
                "Stand-in Pi",
                ".pi",
                "import:\n  instructions: \".pi/agent/AGENTS.md\"\n",
            ),
        ),
        (
            "standin-plain",
            listed("standin-plain", "Stand-in plain", ".plain", ""),
        ),
        (
            "standin-skills",
            listed(
                "standin-skills",
                "Stand-in skills",
                ".skills",
                "import:\n  skills: \".skills/skills\"\n",
            ),
        ),
        (
            "standin-gone",
            listed(
                "standin-gone",
                "Stand-in gone",
                ".gone",
                "import:\n  instructions: \".gone/AGENTS.md\"\n",
            ),
        ),
        (
            "standin-dir",
            listed(
                "standin-dir",
                "Stand-in folder",
                ".dir",
                "import:\n  instructions: \".dir/AGENTS.md\"\n",
            ),
        ),
        (
            "standin-two",
            listed(
                "standin-two",
                "Stand-in two",
                ".two",
                "import:\n  instructions: \".two/RULES.md\"\n",
            ),
        ),
    ];
    let named: Vec<(&str, &str)> = files
        .iter()
        .map(|(id, text)| (*id, text.as_str()))
        .collect();
    Box::leak(Box::new(Registry::build(&named).unwrap()))
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Every entry under a folder with its size and modified time, links listed as links.
/// Input: the folder. Output: the entries, sorted.
fn listing(folder: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut found = Vec::new();
    let mut waiting = vec![folder.to_path_buf()];
    while let Some(at) = waiting.pop() {
        for entry in fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                waiting.push(path.clone());
            }
            found.push((path, metadata.len(), metadata.modified().unwrap()));
        }
    }
    found.sort();
    found
}

/// A temporary owner home holding the Pi stand-in's file, the second stand-in's file as a
/// link to a dotfiles folder outside the home, and a folder where the folder stand-in names
/// a file. Output: the guard, the canonical home and the canonical dotfiles folder.
fn owner_home() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("home");
    let dotfiles = root.join("dotfiles");
    write(&home.join(".pi/agent/AGENTS.md"), "PI-RULE\n");
    write(&dotfiles.join("RULES.md"), "LINKED-RULE\n");
    fs::create_dir_all(home.join(".two")).unwrap();
    std::os::unix::fs::symlink(dotfiles.join("RULES.md"), home.join(".two/RULES.md")).unwrap();
    fs::create_dir_all(home.join(".dir/AGENTS.md")).unwrap();
    fs::create_dir_all(home.join(".skills/skills")).unwrap();
    (directory, home, dotfiles)
}

/// H-import (owner yes 20:1x, DEC-106): a harness's own home instruction file is offered
/// only when its manifest names one and it is a file there (through a link too), in catalog
/// order, with the path as the manifest names it and its size; a missing file, a folder, a
/// manifest with skills only or with no import block offers none, and listing changes nothing.
#[test]
fn a_harness_home_instruction_file_is_offered_only_when_its_manifest_names_one_and_it_is_there() {
    let (_directory, home, _dotfiles) = owner_home();
    let registry = registry();
    let before = listing(&home);

    let offered = sources(&home, registry);

    let order: Vec<&str> = registry
        .all()
        .iter()
        .map(|harness| harness.manifest().id.as_str())
        .filter(|id| ["standin-pi", "standin-two"].contains(id))
        .collect();
    let expected: Vec<Source> = order
        .iter()
        .map(|id| match *id {
            "standin-pi" => Source {
                harness: "standin-pi".into(),
                name: "Stand-in Pi".into(),
                path: ".pi/agent/AGENTS.md".into(),
                bytes: 8,
            },
            _ => Source {
                harness: "standin-two".into(),
                name: "Stand-in two".into(),
                path: ".two/RULES.md".into(),
                bytes: 12,
            },
        })
        .collect();
    assert_eq!(offered, expected);
    assert_eq!(
        listing(&home),
        before,
        "listing the sources changes nothing"
    );
}

/// H-import: a source is read like a file the owner picked, through a link to a file the
/// owner owns outside the home too, and only on its harness's own path: an unknown harness or
/// one whose manifest names no instruction file is refused, a missing file says so, a folder
/// is unsafe, and the read moves, writes and adds nothing in the home.
#[test]
fn a_source_is_read_like_a_picked_file_and_never_written() {
    let (_directory, home, dotfiles) = owner_home();
    let registry = registry();
    let places = Places::new(Vec::new(), Some(home.clone()));
    let before = listing(&home);
    let linked_before = listing(&dotfiles);

    assert_eq!(
        read(&home, registry, &places, "standin-pi").ok().as_deref(),
        Some("PI-RULE\n")
    );
    assert_eq!(
        read(&home, registry, &places, "standin-two")
            .ok()
            .as_deref(),
        Some("LINKED-RULE\n")
    );
    for (harness, code) in [
        ("nobody", "process_request_invalid"),
        ("standin-plain", "process_request_invalid"),
        ("standin-skills", "process_request_invalid"),
        ("standin-gone", "process_import_missing"),
        ("standin-dir", "process_import_unsafe"),
    ] {
        assert_eq!(
            read(&home, registry, &places, harness).unwrap_err().0,
            code,
            "{harness}"
        );
    }
    assert_eq!(listing(&home), before);
    assert_eq!(listing(&dotfiles), linked_before);

    write(
        &home.join(".pi/agent/AGENTS.md"),
        &"a".repeat(crate::agent_home::TEXT_BYTES_MAX + 1),
    );

    assert_eq!(
        read(&home, registry, &places, "standin-pi").unwrap_err().0,
        "process_text_too_large"
    );
}

/// H-import, Q5 (leader 20:28): a source whose file, or a link or linked folder on its way,
/// lies where agents can write (inside a CrazyAgent project) is refused, so agent-written
/// text never reaches the owner's import as the harness's own file.
#[test]
fn a_source_that_leads_into_a_place_agents_can_write_is_refused() {
    let (_directory, home, dotfiles) = owner_home();
    let registry = registry();
    let places = Places::new(Vec::new(), Some(home.clone()));
    let project = dotfiles.parent().unwrap().join("project");
    fs::create_dir_all(project.join(".crazyagent")).unwrap();
    write(&project.join("rules/RULES.md"), "AGENT-WRITTEN\n");

    fs::remove_file(home.join(".two/RULES.md")).unwrap();
    std::os::unix::fs::symlink(project.join("rules/RULES.md"), home.join(".two/RULES.md")).unwrap();

    assert_eq!(
        read(&home, registry, &places, "standin-two")
            .err()
            .map(|refused| refused.0),
        Some("process_import_agents_can_write".to_owned())
    );

    fs::remove_file(home.join(".two/RULES.md")).unwrap();
    std::os::unix::fs::symlink(dotfiles.join("RULES.md"), project.join("link.md")).unwrap();
    std::os::unix::fs::symlink(project.join("link.md"), home.join(".two/RULES.md")).unwrap();

    assert_eq!(
        read(&home, registry, &places, "standin-two")
            .err()
            .map(|refused| refused.0),
        Some("process_import_agents_can_write".to_owned()),
        "a link agents can swap on the way refuses a file outside their reach"
    );

    fs::remove_dir_all(home.join(".pi/agent")).unwrap();
    write(&project.join("agent/AGENTS.md"), "AGENT-WRITTEN\n");
    std::os::unix::fs::symlink(project.join("agent"), home.join(".pi/agent")).unwrap();

    assert_eq!(
        read(&home, registry, &places, "standin-pi")
            .err()
            .map(|refused| refused.0),
        Some("process_import_agents_can_write".to_owned())
    );
}

/// H-import F1 (rv-sec3 21:00, leader 21:06): the file read is the one the checks judged.
/// A project file reached by a link and swapped for a link to an owner secret after it was
/// found is still refused as agent-writable; a safe file swapped the same way is still read
/// as the file that was found; a file missing when looked for is missing, even when a link
/// appears there afterwards. The secret never reaches the preview.
#[test]
fn the_file_read_is_the_file_checked_when_a_link_is_swapped_between_the_two() {
    let secret = |home: &Path| {
        let key = home.join(".ssh/key");
        write(&key, "OWNER-SECRET\n");
        key
    };
    let registry = registry();

    let (_directory, home, dotfiles) = owner_home();
    let places = Places::new(Vec::new(), Some(home.clone()));
    let key = secret(&home);
    let project = dotfiles.parent().unwrap().join("project");
    fs::create_dir_all(project.join(".crazyagent")).unwrap();
    let planted = project.join("rules/RULES.md");
    write(&planted, "AGENT-WRITTEN\n");
    fs::remove_file(home.join(".two/RULES.md")).unwrap();
    std::os::unix::fs::symlink(&planted, home.join(".two/RULES.md")).unwrap();
    let mut swapped = false;

    let read = read_between(&home, registry, &places, "standin-two", || {
        fs::remove_file(&planted).unwrap();
        std::os::unix::fs::symlink(&key, &planted).unwrap();
        swapped = true;
    });

    assert!(swapped);
    assert_eq!(
        read.map_err(|refused| refused.0),
        Err("process_import_agents_can_write".to_owned()),
        "a project file found on the way is refused, whatever it leads to by the read"
    );

    let (_directory, home, dotfiles) = owner_home();
    let places = Places::new(Vec::new(), Some(home.clone()));
    let key = secret(&home);
    let rules = dotfiles.join("RULES.md");
    let mut swapped = false;

    let read = read_between(&home, registry, &places, "standin-two", || {
        fs::remove_file(&rules).unwrap();
        std::os::unix::fs::symlink(&key, &rules).unwrap();
        swapped = true;
    });

    assert!(swapped);
    assert_eq!(
        read.map_err(|refused| refused.0),
        Ok("LINKED-RULE\n".to_owned()),
        "the text is the file that was found, never the one a later link names"
    );

    let (_directory, home, _dotfiles) = owner_home();
    let places = Places::new(Vec::new(), Some(home.clone()));
    let key = secret(&home);
    let gone = home.join(".gone/AGENTS.md");

    let read = read_between(&home, registry, &places, "standin-gone", || {
        fs::create_dir_all(gone.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&key, &gone).unwrap();
    });

    assert_eq!(
        read.map_err(|refused| refused.0),
        Err("process_import_missing".to_owned()),
        "a file missing when looked for is never read later"
    );
}

/// H-import, Q4 (owner batch): no bundled harness offers its home instruction file until
/// the owner says yes to that harness, so a manifest line alone never starts reading a file
/// such as `~/.claude/CLAUDE.md` or `~/.codex/AGENTS.md`. Adding `import.instructions` to a
/// bundled manifest needs the owner's yes and this row's update.
#[test]
fn the_bundled_harnesses_offer_no_home_instruction_file_until_the_owner_says_yes() {
    let bundled = crate::harnesses::registry::registry();
    let naming: Vec<&str> = bundled
        .all()
        .iter()
        .filter(|harness| {
            harness
                .manifest()
                .block("import")
                .and_then(|import| import.get("instructions"))
                .is_some()
        })
        .map(|harness| harness.manifest().id.as_str())
        .collect();

    assert!(naming.is_empty(), "{naming:?}");

    let (_directory, home, _dotfiles) = owner_home();
    write(&home.join(".claude/CLAUDE.md"), "OWNER-GLOBAL\n");
    write(&home.join(".codex/AGENTS.md"), "OWNER-GLOBAL\n");
    assert_eq!(sources(&home, bundled), Vec::new());
    let places = Places::new(Vec::new(), Some(home.clone()));
    for harness in ["claude-code", "codex"] {
        assert_eq!(
            read(&home, bundled, &places, harness).unwrap_err().0,
            "process_request_invalid",
            "{harness}"
        );
    }
}
