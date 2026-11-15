use super::*;
use crate::harnesses::manifest;
use serde_json::json;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::time::{Duration, SystemTime};

const STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin-jsonl-append.yaml");
pub(crate) const STANDIN_PATH: &str = "{home}/standin/records/{session}.jsonl";
const DEEP_PATH: &str = "{home}/agent/sessions/**/*_{session}.jsonl";
pub(crate) const SESSION_ID: &str = "0f0e5c1e-3a5b-4c55-9d1d-2b8f4f6c7a10";
pub(crate) const NEW: Duration = Duration::ZERO;
const OLD: Duration = Duration::from_secs(60);

/// A private home in a scratch folder under the real `/tmp`, removed when dropped. A row
/// writes harness records into it as a harness would.
pub(crate) struct Home {
    root: tempfile::TempDir,
}

impl Home {
    pub(crate) fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-records-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { root }
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.root.path().canonicalize().unwrap()
    }

    /// Writes one file below the home, its folders made first, and dates its last change
    /// `age` back. Inputs: the place below the home, the text and the age. Output: the
    /// file's full path.
    pub(crate) fn write(&self, below: &str, text: &str, age: Duration) -> PathBuf {
        let path = self.path().join(below);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        let file = File::options().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
        path
    }
}

/// The stand-in jsonl-append manifest with its record path replaced. Input: the path as a
/// manifest writes it. Output: the manifest.
pub(crate) fn manifest_with(path: &str) -> Manifest {
    let text = STANDIN.replace(STANDIN_PATH, path);
    manifest::parse(text.as_bytes()).unwrap()
}

/// Reads a record path for the rows' session from the stand-in manifest written with it.
/// Inputs: the home and the path. Output: the pattern or the refusal.
pub(crate) fn pattern(home: &Home, path: &str) -> Result<Pattern, Refused> {
    Pattern::read(&manifest_with(path), &home.path(), SESSION_ID)
}

#[test]
fn the_stand_in_record_path_finds_the_file_its_stand_in_writes() {
    let home = Home::new();
    let below = format!("standin/records/{SESSION_ID}.jsonl");
    let written = home.write(&below, "", NEW);
    let pattern = pattern(&home, STANDIN_PATH).expect("the stand-in record path reads");
    let (_, found, _) = pattern.newest(&[]).expect("the stand-in's record is found");
    assert_eq!(pattern.full(&found), written);
}

#[test]
fn a_deep_record_path_takes_the_newest_session_file_at_any_depth() {
    let home = Home::new();
    home.write(&format!("agent/sessions/a/1_{SESSION_ID}.jsonl"), "", OLD);
    let newest = format!("agent/sessions/b/c/2_{SESSION_ID}.jsonl");
    let newest = home.write(&newest, "", Duration::from_secs(5));
    home.write(&format!("agent/sessions/3_{SESSION_ID}.jsonl"), "", OLD);
    home.write("agent/sessions/4_another-session.jsonl", "", NEW);
    home.write(&format!("agent/sessions/5_{SESSION_ID}.json"), "", NEW);
    home.write(&format!("agent/other/6_{SESSION_ID}.jsonl"), "", NEW);
    let pattern = pattern(&home, DEEP_PATH).expect("the deep record path reads");
    assert_eq!(pattern.found().len(), 3);
    let (_, found, _) = pattern.newest(&[]).expect("a session file is found");
    assert_eq!(pattern.full(&found), newest);
}

#[test]
fn a_link_below_the_home_is_never_followed_to_a_record() {
    let home = Home::new();
    let away = Home::new();
    away.write(&format!("records/{SESSION_ID}.jsonl"), "{}\n", NEW);
    let target = format!("records/9_{SESSION_ID}.jsonl");
    let target = away.write(&target, "{}\n", NEW);
    std::fs::create_dir_all(home.path().join("agent/sessions")).unwrap();
    let folder_link = home.path().join("agent/sessions/linked");
    symlink(away.path().join("records"), folder_link).unwrap();
    let file_link = format!("agent/sessions/7_{SESSION_ID}.jsonl");
    let file_link = home.path().join(file_link);
    symlink(&target, file_link).unwrap();
    symlink(away.path(), home.path().join("standin")).unwrap();
    let deep = pattern(&home, DEEP_PATH).expect("the deep record path reads");
    assert_eq!(deep.found().len(), 0);
    assert!(deep.newest(&[]).is_none());
    let standin = pattern(&home, STANDIN_PATH).expect("the stand-in record path reads");
    assert_eq!(standin.found().len(), 0);
    assert!(standin.newest(&[]).is_none());
}

#[test]
fn a_file_present_before_the_start_is_passed_over_whatever_its_age() {
    let home = Home::new();
    let below = format!("standin/records/{SESSION_ID}.jsonl");
    let earlier = home.write(&below, "", NEW);
    let pattern = pattern(&home, STANDIN_PATH).expect("the stand-in record path reads");
    let keys = pattern.keys();
    assert_eq!(keys.len(), 1);
    assert!(pattern.newest(&keys).is_none());
    let next = home.write("standin/records/next", "", NEW);
    std::fs::rename(next, &earlier).unwrap();
    let (_, found, _) = pattern.newest(&keys).expect("the new file is taken");
    assert_eq!(pattern.full(&found), earlier);
}

#[test]
fn one_search_looks_at_no_more_folder_entries_than_its_bound() {
    let home = Home::new();
    for index in 0..ENTRIES_SCANNED_MAX + 8 {
        let below = format!("agent/sessions/{index}_{SESSION_ID}.jsonl");
        home.write(&below, "", NEW);
    }
    let wide = "{home}/agent/sessions/*_{session}.jsonl";
    let pattern = pattern(&home, wide).expect("the wide record path reads");
    assert_eq!(pattern.found().len(), ENTRIES_SCANNED_MAX - 2);
}

#[test]
fn a_record_deeper_than_the_depth_bound_is_never_found() {
    let home = Home::new();
    let deepest = format!("1/2/3/4/5/6/7/8_{SESSION_ID}.jsonl");
    let deepest = home.write(&deepest, "", Duration::from_secs(5));
    home.write(&format!("1/2/3/4/5/6/7/8/9_{SESSION_ID}.jsonl"), "", NEW);
    let anywhere = "{home}/**/*_{session}.jsonl";
    let pattern = pattern(&home, anywhere).expect("the record path reads");
    assert_eq!(pattern.found().len(), 1);
    let (_, found, _) = pattern.newest(&[]).expect("the bound record is found");
    assert_eq!(pattern.full(&found), deepest);
}

#[test]
fn a_typed_prompt_is_found_in_the_first_object_that_holds_it_whole() {
    let objects = [
        json!({"role": "assistant", "text": "Next, type: fix the parser"}),
        json!({"role": "user", "content": [{"type": "text", "text": "fix the parser\n"}]}),
        json!({"role": "assistant", "text": "fix the parser"}),
    ];
    assert_eq!(typed_at(&objects, "  fix the parser "), Some(1));
}
