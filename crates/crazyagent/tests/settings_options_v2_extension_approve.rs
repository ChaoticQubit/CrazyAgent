#![cfg(target_os = "macos")]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::ide::extensions::{self, AskView, Asked};
use crazyagent::ide::fetch::BuildState;
use crazyagent::protocol::ProjectId;
use e2e_support::{Engine, Owner, scratch};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const PROJECT: &str = "v2-ext-approve";
const EXTENSION: &str = "dracula-theme.theme-dracula";
const VERSION: &str = "2.25.1";
const SHA256: &str = "f4d8c28fc64874b1b959cc90dbd39c84d5d35c9cf203e32b7a758dc9abf5b1cb";
const BUILD: &str = "vscodium-1.143.17183";
const FOLDERS_MAX: usize = 64;

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// Records the pinned IDE build kept in a `.local-state/ide-cache` above the crate as the
/// engine's ready build, the state a finished fetch records, and reads it back through the
/// owner protocol, so the owner's IDE open starts a real server from it. Inputs: the
/// engine and its owner. Output: none.
async fn ready_build(engine: &Engine, owner: &Owner) {
    let build = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|folder| folder.join(".local-state").join("ide-cache").join(BUILD))
        .find(|build| build.join("out").join("server-main.js").is_file())
        .expect("no pinned VSCodium reh-web build in a .local-state/ide-cache above the crate");
    let ready = BuildState::Ready {
        version: BUILD.trim_start_matches("vscodium-").to_owned(),
        root: fs::canonicalize(build).unwrap(),
    };
    let ide = engine.path().join("ide");
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&ide)
        .unwrap();
    fs::write(
        ide.join("build-state.json"),
        serde_json::to_vec(&ready).unwrap(),
    )
    .unwrap();
    let client = owner.controller.owner_client().unwrap();
    assert_eq!(client.ide_tab_state(false).await.unwrap().build, ready);
}

/// The versions the IDE server is let install for one extension, from the locked
/// `extensions.allowed` setting the engine writes before each server start.
/// Input: the engine. Output: the allowed versions, none when it is not allowed.
fn allowed(engine: &Engine, id: &str) -> Option<serde_json::Value> {
    let setting = extensions::allowed_setting(engine.path());
    setting["extensions.allowed"]
        .as_object()
        .expect("the setting lists the allowed extensions")
        .get(id)
        .cloned()
}

/// Finds the files below a folder with one name, walking at most [`FOLDERS_MAX`] folders.
/// Inputs: the folder and the file name. Output: every match.
fn named(top: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![top.to_path_buf()];
    let mut seen = 0;
    while let Some(folder) = folders.pop() {
        seen += 1;
        assert!(
            seen <= FOLDERS_MAX,
            "{} is deeper than expected",
            top.display()
        );
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                folders.push(entry.path());
            } else if kind.is_file() && entry.file_name() == name {
                found.push(entry.path());
            }
        }
    }
    found
}

/// The manifest of the pinned extension the IDE server installed in its extensions
/// folder, read from the folder's top-level installs. Input: the extensions folder.
/// Output: the installed manifests of that extension.
fn installed(extensions: &Path) -> Vec<serde_json::Value> {
    fs::read_dir(extensions)
        .unwrap()
        .map(|entry| entry.unwrap().path().join("package.json"))
        .filter(|manifest| manifest.is_file())
        .map(|manifest| -> serde_json::Value {
            serde_json::from_str(&fs::read_to_string(manifest).unwrap()).unwrap()
        })
        .filter(|manifest| {
            manifest["publisher"].as_str().map(str::to_ascii_lowercase)
                == Some("dracula-theme".to_owned())
                && manifest["name"] == "theme-dracula"
        })
        .collect()
}

/// The extension Approve option end to end. The owner approves the IDE's ask for one
/// small theme-only extension with no dependencies and no native code; the engine reads
/// its metadata, downloads its VSIX and checks it against the registry's sha256, then pins
/// that exact version and hash, keeps the VSIX and lets the IDE install it. The next IDE
/// start installs it, and the pin survives a restart.
///
/// Network: this test reaches Open VSX through the product's own `Registry::open_vsx`
/// path, the metadata at `open-vsx.org` and the VSIX and its `.sha256` on Open VSX's file
/// host `openvsx.eclipsecontent.org`, which `open-vsx.org` redirects to. No other host
/// (owner authorization 2026-10-28). Without that network the test fails as UNAVAILABLE.
#[tokio::test(flavor = "multi_thread")]
async fn extension_approve_pins_the_verified_version_and_the_ide_installs_it() {
    let mut engine = Engine::start("ca-v2opt-ext-approve-");
    let mut owner = Owner::attach(&engine).await;
    let disk = scratch("ca-v2opt-ext-approve-home-");
    let home = disk.path().join("Projects");
    fs::create_dir(&home).unwrap();
    assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
    assert_eq!(
        owner.create_project(PROJECT, "Buoy extensions").await,
        OwnerResult::Applied
    );
    let project = ProjectId::new(PROJECT).unwrap();
    ready_build(&engine, &owner).await;

    assert_eq!(
        extensions::ask(engine.path(), &project, EXTENSION, VERSION, now_ms()).unwrap(),
        Asked::Confirm
    );
    assert_eq!(allowed(&engine, EXTENSION), None);
    let client = owner.controller.owner_client().unwrap();
    let approved = match client
        .ide_extension_approve(project.clone(), EXTENSION.into(), VERSION.into())
        .await
    {
        Ok(approved) => approved,
        Err(error) if error.0 == "ide_extension_download_failed" => panic!(
            "UNAVAILABLE: the extension Approve e2e needs HTTPS to open-vsx.org and \
             openvsx.eclipsecontent.org ({})",
            error.0
        ),
        Err(error) => panic!("the approval was refused: {}", error.0),
    };
    assert_eq!(
        (
            approved.id.as_str(),
            approved.version.as_str(),
            approved.sha256.as_str()
        ),
        (EXTENSION, VERSION, SHA256),
        "the exact version and its verified sha256 are pinned"
    );
    assert_eq!(
        allowed(&engine, EXTENSION),
        Some(serde_json::json!([VERSION])),
        "the IDE is let install that exact version alone"
    );
    let vsix = named(
        &engine.path().join("ide"),
        &format!("{EXTENSION}-{VERSION}.vsix"),
    );
    assert_eq!(vsix.len(), 1, "the verified VSIX is kept once: {vsix:?}");
    let digest: String = Sha256::digest(fs::read(&vsix[0]).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(digest, SHA256, "the kept VSIX is the one verified");
    assert_eq!(
        client.ide_extension_asks(project.clone()).await.unwrap(),
        Vec::<AskView>::new(),
        "the approval answers the ask"
    );
    assert_eq!(
        extensions::ask(engine.path(), &project, EXTENSION, VERSION, now_ms()).unwrap(),
        Asked::Approved,
        "an approved extension needs no second confirm"
    );

    let opened = client.ide_tab_open(project.clone()).await.unwrap();
    assert!(opened.url.starts_with("http://127.0.0.1:"), "{opened:?}");
    let manifests = installed(&engine.path().join("ide").join(PROJECT).join("extensions"));
    assert_eq!(manifests.len(), 1, "{manifests:?}");
    assert_eq!(
        manifests[0]["version"], VERSION,
        "the IDE start installed the pinned version"
    );
    assert!(client.ide_tab_stop(project.clone()).await.unwrap());

    drop(owner);
    engine.restart();
    assert_eq!(
        allowed(&engine, EXTENSION),
        Some(serde_json::json!([VERSION])),
        "the pin survives a restart"
    );
}
