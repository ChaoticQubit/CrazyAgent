#![cfg(target_os = "linux")]
use super::*;

/// Writes one object through a confined step that may write objects. Inputs: the step and
/// the object's bytes. Output: the step's exit code and the object id it printed, or none
/// when git did not start.
fn write_object(repo: &git::Repo, bytes: &[u8]) -> Option<(i32, String)> {
    let output = repo
        .exec(&["hash-object", "-w", "--stdin"], Some(bytes), &[], 4096)
        .ok()?;
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Some((output.code, id))
}

/// An owner whose `.git/objects` is a link to a real folder still lands: a confined step that
/// may write objects writes them in the folder the link names.
#[test]
fn a_linked_object_store_still_takes_a_confined_steps_objects() {
    let case = Case::new();
    let objects = case.repo.join(".git").join("objects");
    let real = case.repo.parent().unwrap().join("objects-real");
    std::fs::rename(&objects, &real).unwrap();
    std::os::unix::fs::symlink(&real, &objects).unwrap();
    let repo = confined_agent(&case, git::Writes::Objects);
    let (code, id) = write_object(&repo, b"linked object store 3b8d\n").unwrap();
    assert_eq!(code, 0);
    assert_eq!(id.len(), 40, "{id:?}");
    assert!(real.join(&id[..2]).join(&id[2..]).is_file(), "{id}");
}

/// An object store swapped for a link to another folder after the confined step was built
/// takes no object there: the step is refused before git runs.
#[test]
fn an_object_store_swapped_for_a_link_after_the_step_is_built_takes_nothing() {
    let case = Case::new();
    let repo = confined_agent(&case, git::Writes::Objects);
    let objects = case.repo.join(".git").join("objects");
    let elsewhere = case.repo.parent().unwrap().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::rename(&objects, objects.with_file_name("objects-aside")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &objects).unwrap();
    let written = write_object(&repo, b"swapped object store 9e41\n");
    let landed: Vec<_> = std::fs::read_dir(&elsewhere).unwrap().collect();
    assert!(landed.is_empty(), "objects landed at the link's target");
    assert!(
        written.as_ref().is_none_or(|(code, _)| *code != 0),
        "{written:?}"
    );
}
