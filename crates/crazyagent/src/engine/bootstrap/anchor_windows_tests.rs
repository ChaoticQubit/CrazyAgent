use super::{
    Anchor, EngineService, Identity, LOCATOR, Locator, PinnedLocator, SocketIdentity,
    check_database, check_generation, check_socket, discover_owner, parse_locator,
    reported_engine_version,
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
    let done = crate::spawn_gate::output(&mut command).unwrap();
    assert!(
        done.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&done.stderr)
    );
}

/// Makes `link` a junction to `target`. Inputs: the two folders. Output: none.
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

/// A recorded Windows endpoint: the random part filled with `seed` and a nonzero pin.
/// Input: the seed. Output: the endpoint.
fn endpoint(seed: u8) -> SocketIdentity {
    SocketIdentity {
        random: [seed; 16],
        pid: 7,
        created: 9,
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
        owner_socket: endpoint(1),
        ordinary_name: "e.sock".into(),
        ordinary_socket: endpoint(2),
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
    let (temp, base) = plain_temp();
    let path = base.join("data");
    let engine = product_anchor(&path).await;
    let short = temp.path().join("data");
    assert!(
        short.to_str().unwrap().contains('~'),
        "the hosted runner's temp folder is spelled with a short name: {}",
        short.display()
    );
    let upper = PathBuf::from(path.to_str().unwrap().to_uppercase());
    for spelling in [short, upper] {
        let anchor = Anchor::open(&spelling).expect("another spelling of the anchor opens");
        assert_eq!(anchor.path, path, "the stored path is the plain long name");
        anchor.verify().unwrap();
        assert!(
            crate::sandbox::windows_clean_text(anchor.path.to_str().unwrap()),
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
        "owner_endpoint_replaced_or_unsafe",
        "an intact generation reaches the endpoint check, which finds no pipes file in it"
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

/// W3-4b D0 (#717 Q5 condition 3): with the early refusal gone, `start` on a product-made
/// anchor goes on to `start_inner`, which makes `engine.db`, serves the engine and
/// publishes the locator, and its shutdown removes the locator again.
#[tokio::test(flavor = "multi_thread")]
async fn a_windows_engine_start_reaches_start_inner_and_serves() {
    let (_temp, base) = plain_temp();
    let path = base.join("data");
    drop(private::store_dir(&path, private::Store::Database).unwrap());
    let config = settings(&base);
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(&path, "e.sock", &config, &stopped)
        .await
        .expect("a Windows start reaches start_inner and serves");
    assert!(std::fs::symlink_metadata(path.join("engine.db")).is_ok());
    assert!(std::fs::symlink_metadata(path.join(LOCATOR)).is_ok());
    service.ensure_ready(&stopped).unwrap();
    service.shutdown().await.unwrap();
    assert!(std::fs::symlink_metadata(path.join(LOCATOR)).is_err());
}

/// W3-4b D0: `start_with_settings_sources`, the other caller of the early refusal, also
/// reaches `start_inner` and serves, with its settings read from the two named files.
#[tokio::test(flavor = "multi_thread")]
async fn a_windows_start_with_settings_sources_reaches_start_inner_and_serves() {
    let (_temp, base) = plain_temp();
    let path = base.join("data");
    drop(private::store_dir(&path, private::Store::Database).unwrap());
    let global = base.join("g");
    std::fs::write(&global, "").unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        &path,
        "e.sock",
        &global,
        &base.join("p"),
        &stopped,
    )
    .await
    .expect("a Windows start with settings sources reaches start_inner and serves");
    assert!(std::fs::symlink_metadata(path.join("engine.db")).is_ok());
    service.ensure_ready(&stopped).unwrap();
    service.shutdown().await.unwrap();
}

/// W3-4b D1 (rv-sec2 #717 Q2/Q3): a locator whose owner or ordinary endpoint has a zero
/// process ID, a zero creation time or an all-zero random part is refused, while one
/// nonzero random byte parses.
#[test]
fn a_locator_endpoint_with_a_zero_part_is_refused() {
    let parse = |value: &Locator| parse_locator(&serde_json::to_vec(value).unwrap()).map(|_| ());
    let base = locator(
        "o01234567",
        Identity {
            device: 1,
            inode: 3,
        },
    );
    parse(&base).expect("the control parses");
    let mut one = base.clone();
    one.ordinary_socket.random = [0; 16];
    one.ordinary_socket.random[15] = 1;
    parse(&one).expect("one nonzero random byte parses");
    let changes: [fn(&mut Locator); 6] = [
        |value| value.owner_socket.pid = 0,
        |value| value.owner_socket.created = 0,
        |value| value.owner_socket.random = [0; 16],
        |value| value.ordinary_socket.pid = 0,
        |value| value.ordinary_socket.created = 0,
        |value| value.ordinary_socket.random = [0; 16],
    ];
    for (case, change) in changes.iter().enumerate() {
        let mut value = base.clone();
        change(&mut value);
        assert_eq!(
            refusal(parse(&value)),
            "owner_locator_invalid",
            "case {case}"
        );
    }
}

/// The names a stopped engine must not leave in its anchor: the locator, the pipes file, a
/// generation folder or a capability file. Input: the anchor path. Output: those names.
fn engine_leftovers(path: &Path) -> Vec<String> {
    std::fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name == LOCATOR
                || name == "pipes"
                || name.ends_with(".cap")
                || (name.len() == 9 && name.starts_with('o'))
        })
        .collect()
}

/// W3-4b D2 and D3, the first end-to-end row (PLAN-0328:87): a started engine's ordinary
/// endpoint passes `check_socket` as recorded and fails with any part changed; an owner
/// client connects through the ordinary endpoint; `check_engine` answers Ready with this
/// version; and shutdown leaves no locator, pipes file, generation or capability file.
#[tokio::test(flavor = "multi_thread")]
async fn a_started_windows_engine_serves_an_owner_client_and_stops_clean() {
    let (_temp, base) = plain_temp();
    let path = base.join("data");
    drop(private::store_dir(&path, private::Store::Database).unwrap());
    let config = settings(&base);
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(&path, "e.sock", &config, &stopped)
        .await
        .expect("a Windows start serves");
    let recorded = service.locator.as_ref().unwrap().value.ordinary_socket;
    let ordinary = path.join("e.sock");
    check_socket(&service.anchor, &ordinary, recorded).expect("the live endpoint passes");
    let mut random = recorded.random;
    random[0] ^= 1;
    for changed in [
        SocketIdentity {
            pid: recorded.pid + 1,
            ..recorded
        },
        SocketIdentity {
            created: recorded.created + 1,
            ..recorded
        },
        SocketIdentity { random, ..recorded },
    ] {
        assert_eq!(
            refusal(check_socket(&service.anchor, &ordinary, changed)),
            "owner_endpoint_replaced_or_unsafe"
        );
    }
    let timeout = Duration::from_secs(5);
    let (client, owner, identity) = crate::engine::Client::new(ordinary.clone(), timeout)
        .unwrap()
        .with_owner(OwnerClientKind::Tui)
        .await
        .expect("an owner client connects through the ordinary endpoint");
    assert_eq!(identity.engine_version(), crate::version::VERSION);
    assert_eq!(
        reported_engine_version(&path).as_deref(),
        Some(crate::version::VERSION)
    );
    drop((client, owner));
    assert_eq!(
        crate::service::check_engine(&path, OwnerClientKind::Tui, timeout).await,
        crate::service::EngineCheck::Ready {
            version: crate::version::VERSION.into()
        }
    );
    service.shutdown().await.unwrap();
    assert_eq!(
        refusal(check_socket(&service.anchor, &ordinary, recorded)),
        "owner_endpoint_replaced_or_unsafe"
    );
    assert!(
        engine_leftovers(&path).is_empty(),
        "{:?}",
        engine_leftovers(&path)
    );
    assert_eq!(reported_engine_version(&path), None);
}

/// rv-sec2 #738 Q5, D0/D3: a second start on the anchor while the first serves is refused at
/// the engine database lock, before it publishes: the first engine's pipes file and locator
/// keep their bytes, and its ordinary endpoint still answers.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_windows_start_on_a_served_anchor_is_refused_at_the_database_lock() {
    let (_temp, base) = plain_temp();
    let path = base.join("data");
    drop(private::store_dir(&path, private::Store::Database).unwrap());
    let config = settings(&base);
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut first = EngineService::start(&path, "e.sock", &config, &stopped)
        .await
        .expect("the first start serves");
    let pipes = std::fs::read(path.join("pipes")).unwrap();
    let published = std::fs::read(path.join(LOCATOR)).unwrap();
    let refused = refusal(EngineService::start(&path, "e.sock", &config, &stopped).await);
    assert!(
        refused.starts_with("database_lock_acquire_failed"),
        "{refused}"
    );
    assert_eq!(std::fs::read(path.join("pipes")).unwrap(), pipes);
    assert_eq!(std::fs::read(path.join(LOCATOR)).unwrap(), published);
    assert!(crate::ipc::owner_answering(&path.join("e.sock")));
    first.shutdown().await.unwrap();
}

/// W3-4b rows 6, 7, 8 and rv-sec2 #738 Q1-b, Q5-c: a start over what an engine killed
/// without its cleanup leaves (its locator, its pipes file and a generation folder) starts
/// once the lock is released: the stale locator is renamed over while a reader holds it,
/// whose pin then fails `verify`; the stale pipes file is replaced and its endpoint fails
/// `check_socket`; the leftover private generation is swept and a foreign one is kept.
#[tokio::test(flavor = "multi_thread")]
async fn a_start_over_a_killed_engines_leftovers_replaces_them_and_serves() {
    let (_temp, base) = plain_temp();
    let path = base.join("data");
    drop(private::store_dir(&path, private::Store::Database).unwrap());
    let config = settings(&base);
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut first = EngineService::start(&path, "e.sock", &config, &stopped)
        .await
        .expect("the first start serves");
    let killed_endpoint = first.locator.as_ref().unwrap().value.ordinary_socket;
    let killed_pipes = std::fs::read(path.join("pipes")).unwrap();
    let killed_locator = std::fs::read(path.join(LOCATOR)).unwrap();
    first.shutdown().await.unwrap();
    drop(first);
    assert!(
        crate::storage::engine_lock_released(&path),
        "Q5-c: the lock is free first"
    );
    private_bytes(&path.join(LOCATOR), &killed_locator);
    let mut pipes = private::secret_file(&path.join("pipes")).unwrap();
    std::io::Write::write_all(&mut pipes, &killed_pipes).unwrap();
    drop(pipes);
    drop(private::dir(&path.join("o0badbad0")).unwrap());
    std::fs::create_dir(path.join("o12345678")).unwrap();
    grant_everyone(&path.join("o12345678"));
    let reader = PinnedLocator::read(&Anchor::open(&path).unwrap())
        .unwrap()
        .expect("the killed engine's locator reads");
    let mut second = EngineService::start(&path, "e.sock", &config, &stopped)
        .await
        .expect("a start over a killed engine's leftovers serves");
    let anchor = Anchor::open(&path).unwrap();
    assert_eq!(
        refusal(reader.verify(&anchor)),
        "owner_locator_replaced_or_unsafe",
        "Q1-b: the publish renamed over the held stale locator"
    );
    let published = second.locator.as_ref().unwrap();
    assert_ne!(published.bytes, killed_locator);
    assert_eq!(
        refusal(check_socket(&anchor, &path.join("e.sock"), killed_endpoint)),
        "owner_endpoint_replaced_or_unsafe"
    );
    check_socket(
        &anchor,
        &path.join("e.sock"),
        published.value.ordinary_socket,
    )
    .expect("the new endpoint passes");
    assert!(std::fs::symlink_metadata(path.join("o0badbad0")).is_err());
    assert!(std::fs::symlink_metadata(path.join("o12345678")).is_ok());
    second.shutdown().await.unwrap();
}
