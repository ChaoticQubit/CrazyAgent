use super::{
    Anchor, EngineService, Identity, LOCATOR, Locator, PinnedLocator, check_database,
    check_generation, discover_owner,
};
use crate::engine::Engine;
use crate::engine::owner::OwnerClientKind;
use crate::platform::FileExt;
use crazyagent_win_sandbox::private;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// A fresh temp folder in its plain long-name spelling, with no short name, junction or
/// other case on the way. Output: the temp folder, kept alive by the caller, and that path.
fn plain_temp() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let plain = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    (temp, plain)
}

/// The settings an engine opens under, read from two files beside the anchor, not in it.
/// Input: the folder that holds the anchor. Output: the settings.
fn settings(base: &Path) -> crate::config::EffectiveConfig {
    crate::config::resolve_config(&base.join("g"), &base.join("p")).unwrap()
}

/// An engine data root made as the product makes one: the engine opens `engine.db` in it,
/// so storage makes the store folder with its label and holds the folder and database.
/// Input: the root path, which must not exist yet. Output: the open engine.
async fn product_anchor(path: &Path) -> Engine {
    let base = path.parent().unwrap();
    Engine::open(&path.join("engine.db"), settings(base))
        .await
        .unwrap()
}

/// Runs one Windows command and requires it to succeed. Inputs: the program and its
/// arguments. Output: none; a failure panics with the command's error output.
fn run(program: &str, args: &[&OsStr]) {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let done = kernel_processes::spawn_gate::output(&mut command).unwrap();
    assert!(
        done.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&done.stderr)
    );
}

/// Makes `link` a junction to `target`. Inputs: the two folders. Output: none.
/// The 8.3 alias Windows gave `long`, read through cmd's `%~s` form, which asks
/// GetShortPathNameW, so a row names a short spelling it made on purpose (CR-1341 HIGH 1).
/// Input: an existing folder with a long name. Output: the alias, or None when this volume
/// makes no 8.3 names, which the row then reports as a named SKIP.
fn short_alias(long: &Path) -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("cmd");
    command
        .raw_arg(format!(
            "/D /C for %I in (\"{}\") do @echo %~sI",
            long.display()
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let done = kernel_processes::spawn_gate::output(&mut command).unwrap();
    assert!(done.status.success(), "{done:?}");
    let short = PathBuf::from(String::from_utf8(done.stdout).unwrap().trim());
    assert!(short.is_absolute(), "{}", short.display());
    (short != long && short.to_str()?.contains('~')).then_some(short)
}

fn junction(link: &Path, target: &Path) {
    run(
        "cmd",
        &[
            "/C".as_ref(),
            "mklink".as_ref(),
            "/J".as_ref(),
            link.as_os_str(),
            target.as_os_str(),
        ],
    );
}

/// Grants everyone read on `path`, a foreign entry the private judge refuses.
/// Input: the path. Output: none.
fn grant_everyone(path: &Path) {
    run(
        "icacls",
        &[path.as_os_str(), "/grant".as_ref(), "*S-1-1-0:(R)".as_ref()],
    );
}

/// The refusal code a call returned. Input: the result. Output: its code text.
fn refusal<T>(result: Result<T, crate::engine::EngineError>) -> String {
    match result {
        Ok(_) => panic!("the call was accepted"),
        Err(error) => error.0,
    }
}

/// A locator that parses, naming `generation` with the given folder identity. Inputs: the
/// generation name and its identity. Output: the locator.
fn locator(generation: &str, generation_identity: Identity) -> Locator {
    let id = Identity {
        device: 1,
        inode: 2,
    };
    Locator {
        version: 1,
        database: id,
        db_instance: "a".repeat(64),
        engine_epoch: "b".repeat(64),
        generation: generation.into(),
        generation_identity,
        owner_socket: id,
        ordinary_name: "e.sock".into(),
        ordinary_socket: id,
        engine_version: String::new(),
    }
}

/// Writes `bytes` as a private file at `path`, as W1 makes every private store file.
/// Inputs: the path and the bytes. Output: none.
fn private_bytes(path: &Path, bytes: &[u8]) {
    let file = private::file(path, true).unwrap();
    file.write_all_at(bytes, 0).unwrap();
    file.sync_all().unwrap();
}

/// A product-made anchor with its engine closed, so nothing holds the folder. Input: the
/// folder that holds it. Output: the anchor path.
async fn closed_anchor(base: &Path) -> PathBuf {
    let path = base.join("data");
    product_anchor(&path).await.close().await.unwrap();
    path
}

#[tokio::test]
async fn a_product_made_anchor_opens_plain_and_verifies() {
    let (_temp, base) = plain_temp();
    let path = base.join("data");
    let engine = product_anchor(&path).await;
    let anchor = Anchor::open(&path).expect("the engine's own data root opens as its anchor");
    assert_eq!(
        anchor.path, path,
        "the anchor is stored in its plain spelling"
    );
    anchor.verify().expect("the held anchor verifies");
    engine.close().await.unwrap();
}

#[tokio::test]
async fn an_anchor_given_as_a_short_name_or_another_case_opens_at_its_plain_long_name() {
    let (_temp, base) = plain_temp();
    let long = base.join("a long anchor folder name");
    std::fs::create_dir(&long).unwrap();
    let path = long.join("data");
    let engine = product_anchor(&path).await;
    let Some(alias) = short_alias(&long) else {
        println!("SKIP an_anchor_given_as_a_short_name: this volume makes no 8.3 names");
        engine.close().await.unwrap();
        return;
    };
    let short = alias.join("data");
    let upper = PathBuf::from(path.to_str().unwrap().to_uppercase());
    for spelling in [short, upper] {
        let anchor = Anchor::open(&spelling).expect("another spelling of the anchor opens");
        assert_eq!(anchor.path, path, "the stored path is the plain long name");
        anchor.verify().unwrap();
        assert!(
            kernel_sandbox::paths::windows_clean_text(anchor.path.to_str().unwrap()),
            "the stored path is plain text the sandbox accepts"
        );
    }
    engine.close().await.unwrap();
}

#[tokio::test]
async fn a_junction_ancestor_opens_at_the_real_place_and_a_junction_anchor_is_refused() {
    let (_temp, base) = plain_temp();
    let real = base.join("real");
    std::fs::create_dir(&real).unwrap();
    let path = real.join("data");
    let engine = product_anchor(&path).await;
    junction(&base.join("link"), &real);
    let through = Anchor::open(&base.join("link").join("data"))
        .expect("an anchor under a junction ancestor opens");
    assert_eq!(through.path, path, "the stored path is the real place");
    junction(&base.join("leaf"), &path);
    assert_eq!(
        refusal(Anchor::open(&base.join("leaf"))),
        "owner_anchor_requires_private_directory"
    );
    engine.close().await.unwrap();
}

#[tokio::test]
async fn an_anchor_with_a_foreign_entry_is_refused() {
    let (_temp, base) = plain_temp();
    let path = closed_anchor(&base).await;
    Anchor::open(&path).expect("the private anchor opens before the grant");
    grant_everyone(&path);
    assert_eq!(
        refusal(Anchor::open(&path)),
        "owner_anchor_requires_private_directory"
    );
}

#[tokio::test]
async fn an_anchor_replaced_while_no_one_holds_it_fails_verify() {
    let (_temp, base) = plain_temp();
    let path = closed_anchor(&base).await;
    let first = Anchor::open(&path).unwrap();
    let recorded = Anchor {
        supplied: path.clone(),
        path: path.clone(),
        directory: private::dir(&base.join("spare")).unwrap(),
        identity: first.identity,
    };
    recorded
        .verify()
        .expect("the recorded anchor verifies while its folder is the one opened");
    drop(first);
    std::fs::rename(&path, base.join("old")).unwrap();
    drop(private::dir(&path).unwrap());
    assert_eq!(
        refusal(recorded.verify()),
        "owner_anchor_replaced_or_unsafe",
        "a new folder at the anchor's path is not the anchor that was opened"
    );
}

#[tokio::test]
async fn a_missing_locator_reads_as_none_and_a_private_one_round_trips() {
    let (_temp, base) = plain_temp();
    let path = closed_anchor(&base).await;
    let anchor = Anchor::open(&path).unwrap();
    assert!(PinnedLocator::read(&anchor).unwrap().is_none());
    let value = locator(
        "o01234567",
        Identity {
            device: 3,
            inode: 4,
        },
    );
    private_bytes(&path.join(LOCATOR), &serde_json::to_vec(&value).unwrap());
    let pinned = PinnedLocator::read(&anchor)
        .unwrap()
        .expect("a private locator is read");
    assert_eq!(pinned.value.generation, "o01234567");
    pinned.verify(&anchor).unwrap();
}

#[tokio::test]
async fn a_foreign_hard_linked_or_symbolic_link_locator_is_refused() {
    let (_temp, base) = plain_temp();
    let bytes = serde_json::to_vec(&locator(
        "o01234567",
        Identity {
            device: 3,
            inode: 4,
        },
    ))
    .unwrap();
    for case in ["foreign", "hard link", "symbolic link"] {
        let folder = base.join(case.replace(' ', "-"));
        std::fs::create_dir(&folder).unwrap();
        let path = closed_anchor(&folder).await;
        let anchor = Anchor::open(&path).unwrap();
        let named = path.join(LOCATOR);
        match case {
            "foreign" => {
                private_bytes(&named, &bytes);
                grant_everyone(&named);
            }
            "hard link" => {
                private_bytes(&path.join("target"), &bytes);
                std::fs::hard_link(path.join("target"), &named).unwrap();
            }
            _ => {
                private_bytes(&path.join("target"), &bytes);
                std::os::windows::fs::symlink_file(path.join("target"), &named).unwrap();
            }
        }
        assert_eq!(
            refusal(PinnedLocator::read(&anchor)),
            "owner_locator_unsafe",
            "{case}"
        );
    }
}

#[tokio::test]
async fn a_locator_changed_in_place_or_renamed_over_while_held_fails_verify() {
    let (_temp, base) = plain_temp();
    let path = closed_anchor(&base).await;
    let anchor = Anchor::open(&path).unwrap();
    let named = path.join(LOCATOR);
    let first = serde_json::to_vec(&locator(
        "o01234567",
        Identity {
            device: 3,
            inode: 4,
        },
    ))
    .unwrap();
    let second = serde_json::to_vec(&locator(
        "o76543210",
        Identity {
            device: 3,
            inode: 4,
        },
    ))
    .unwrap();
    assert_eq!(first.len(), second.len());
    private_bytes(&named, &first);
    let pinned = PinnedLocator::read(&anchor).unwrap().unwrap();
    let writer = private::file(&named, false).unwrap();
    writer.write_all_at(&second, 0).unwrap();
    assert_eq!(
        refusal(pinned.verify(&anchor)),
        "owner_locator_replaced_or_unsafe",
        "the bytes the engine pinned changed in place"
    );
    drop((pinned, writer));
    let held = PinnedLocator::read(&anchor).unwrap().unwrap();
    private_bytes(&path.join("next"), &first);
    std::fs::rename(path.join("next"), &named)
        .expect("a new locator is renamed over the one a reader holds");
    assert_eq!(
        refusal(held.verify(&anchor)),
        "owner_locator_replaced_or_unsafe",
        "the held locator is no longer the file at its name"
    );
}

#[tokio::test]
async fn the_database_check_passes_while_the_engine_holds_it_and_fails_once_swapped() {
    let (_temp, base) = plain_temp();
    let path = base.join("data");
    let engine = product_anchor(&path).await;
    let anchor = Anchor::open(&path).unwrap();
    let (_, recorded) = anchor
        .entry("engine.db", false, "owner_locator_database_mismatch")
        .expect("the database opens while the engine and its lock hold it");
    check_database(&anchor, recorded).expect("the held database is the recorded one");
    engine.close().await.unwrap();
    drop(anchor);
    std::fs::rename(path.join("engine.db"), base.join("old.db")).unwrap();
    private_bytes(&path.join("engine.db"), b"swapped");
    let anchor = Anchor::open(&path).unwrap();
    assert_eq!(
        refusal(check_database(&anchor, recorded)),
        "owner_locator_database_mismatch"
    );
}

#[tokio::test]
async fn a_replaced_generation_is_refused_before_the_endpoint_check() {
    let (_temp, base) = plain_temp();
    let path = closed_anchor(&base).await;
    let anchor = Anchor::open(&path).unwrap();
    drop(private::dir(&path.join("o01234567")).unwrap());
    let (_, identity) = anchor
        .entry("o01234567", true, "owner_generation_replaced_or_unsafe")
        .unwrap();
    let value = locator("o01234567", identity);
    assert_eq!(
        refusal(check_generation(&anchor, &value)),
        "owner_anchor_unavailable",
        "an intact generation reaches the endpoint check, which still refuses on Windows"
    );
    std::fs::rename(path.join("o01234567"), path.join("old")).unwrap();
    drop(private::dir(&path.join("o01234567")).unwrap());
    assert_eq!(
        refusal(check_generation(&anchor, &value)),
        "owner_generation_replaced_or_unsafe"
    );
}

#[tokio::test]
async fn a_client_on_an_anchor_with_no_locator_finds_no_owner() {
    let (_temp, base) = plain_temp();
    let path = closed_anchor(&base).await;
    assert_eq!(
        refusal(discover_owner(&path, OwnerClientKind::Tui, Duration::from_secs(1)).await),
        "owner_locator_missing"
    );
}

#[tokio::test]
async fn a_windows_engine_start_refuses_before_it_makes_a_database() {
    let (_temp, base) = plain_temp();
    let path = base.join("data");
    drop(private::store_dir(&path, private::Store::Database).unwrap());
    let config = settings(&base);
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    assert_eq!(
        refusal(EngineService::start(&path, "e.sock", &config, &stopped).await),
        "owner_anchor_unavailable"
    );
    assert!(
        std::fs::symlink_metadata(path.join("engine.db")).is_err(),
        "a refused Windows start makes no engine.db"
    );
    Anchor::open(&path).expect("the refusal is the early one: the anchor itself opens");
}
