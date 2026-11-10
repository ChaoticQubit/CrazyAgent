use super::*;
use crate::github::test_server::{Answer, Server};
use serde_json::json;

const NOW: i64 = 1_700_000_000_000;
const ID: &str = "acme.tool";

/// One version a loopback registry lists: the bytes it serves as the VSIX, the sha256 it
/// publishes for them, and a download URL that replaces its own when given.
struct Release {
    version: &'static str,
    served: Vec<u8>,
    published: String,
    download: Option<String>,
}

impl Release {
    fn genuine(version: &'static str, bytes: &[u8]) -> Self {
        Self {
            version,
            served: bytes.to_vec(),
            published: sha256(bytes),
            download: None,
        }
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A real HTTP server on 127.0.0.1 laid out like Open VSX's API for `acme.tool`: version
/// metadata, the newest version's metadata, the VSIX files and their sha256 files.
fn registry(releases: Vec<Release>, latest: &'static str) -> (Server, Registry) {
    let server = Server::start(move |seen, _| {
        let host = seen.header("host").unwrap_or_default().to_owned();
        let base = format!("http://{host}/api/acme/tool");
        let metadata = |release: &Release| {
            let file = format!("{base}/{}/file/{ID}-{}", release.version, release.version);
            Answer::json(
                200,
                json!({
                    "namespace": "acme",
                    "name": "tool",
                    "version": release.version,
                    "files": {
                        "download": release.download.clone().unwrap_or(format!("{file}.vsix")),
                        "sha256": format!("{file}.sha256"),
                    },
                }),
            )
        };
        let path = seen.path.strip_prefix("/api/acme/tool").unwrap_or("");
        if path.is_empty() {
            let newest = releases.iter().find(|release| release.version == latest);
            return newest.map_or(Answer::bytes(404, Vec::new()), metadata);
        }
        for release in &releases {
            let version = format!("/{}", release.version);
            let file = format!("{version}/file/{ID}-{}", release.version);
            if path == version {
                return metadata(release);
            }
            if path == format!("{file}.vsix") {
                return Answer::bytes(200, release.served.clone());
            }
            if path == format!("{file}.sha256") {
                let text = format!("{}  {ID}-{}.vsix\n", release.published, release.version);
                return Answer::bytes(200, text.into_bytes());
            }
        }
        Answer::bytes(404, Vec::new())
    });
    let registry = Registry::at(&format!("http://127.0.0.1:{}/api/", server.port));
    (server, registry)
}

fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ca-ide-ext-")
        .tempdir()
        .unwrap()
}

fn project() -> ProjectId {
    ProjectId::new("p1").unwrap()
}

fn allowed(root: &Path) -> serde_json::Value {
    allowed_setting(root)
        .remove("extensions.allowed")
        .expect("the setting is extensions.allowed")
}

fn cache_files(root: &Path) -> Vec<String> {
    let cache = root.join(IDE).join(FOLDER).join(CACHE);
    let Ok(entries) = std::fs::read_dir(cache) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn allowed_setting_lists_only_approved_pinned_versions() {
    let root = root();
    let project = project();
    let built_in = built_in_version();
    assert_eq!(built_in, "0.1.0");
    assert_eq!(
        allowed(root.path()),
        json!({ BUILT_IN: [built_in] }),
        "with nothing approved only the built-in is allowed"
    );

    let (_server, registry) = registry(vec![Release::genuine("1.0.0", b"tool one")], "1.0.0");
    assert_eq!(
        ask(root.path(), &project, ID, "1.0.0", NOW).unwrap(),
        Asked::Confirm
    );
    assert_eq!(
        ask(root.path(), &project, "acme.other", "2.0.0", NOW).unwrap(),
        Asked::Confirm
    );
    assert_eq!(
        allowed(root.path()),
        json!({ BUILT_IN: [built_in] }),
        "an ask from the IDE allows nothing"
    );

    let pinned = approve(root.path(), &registry, &project, ID, "1.0.0", NOW + 1).unwrap();
    assert_eq!(
        pinned,
        Approved {
            id: ID.to_owned(),
            version: "1.0.0".to_owned(),
            sha256: sha256(b"tool one"),
            approved_ms: NOW + 1,
        }
    );
    assert_eq!(
        allowed(root.path()),
        json!({ ID: ["1.0.0"], BUILT_IN: [built_in] }),
        "the approved extension is allowed at its pinned version only"
    );
    assert_eq!(
        ask(root.path(), &project, ID, "1.0.0", NOW + 2).unwrap(),
        Asked::Approved
    );
    assert_eq!(
        ask(root.path(), &project, ID, LATEST, NOW + 2).unwrap(),
        Asked::Approved
    );
    assert_eq!(cache_files(root.path()), [format!("{ID}-1.0.0.vsix")]);

    let policy = root.path().join(IDE).join(FOLDER).join(POLICY);
    std::fs::write(&policy, b"{ not json").unwrap();
    assert_eq!(
        allowed(root.path()),
        json!({ BUILT_IN: [built_in] }),
        "an unreadable policy allows only the built-in"
    );
    assert_eq!(
        ask(root.path(), &project, ID, "1.0.0", NOW + 3)
            .unwrap_err()
            .0,
        "ide_extensions_unavailable"
    );
}

#[test]
fn vsix_with_a_different_sha256_is_refused_and_not_installed() {
    let root = root();
    let project = project();
    let extensions = root.path().join("server-extensions");
    std::fs::create_dir(&extensions).unwrap();
    let tampered = Release {
        version: "1.0.0",
        served: b"tampered bytes".to_vec(),
        published: sha256(b"genuine bytes"),
        download: None,
    };
    let (server, registry) = registry(vec![tampered], "1.0.0");
    ask(root.path(), &project, ID, "1.0.0", NOW).unwrap();

    assert_eq!(
        approve(root.path(), &registry, &project, ID, "1.0.0", NOW + 1)
            .unwrap_err()
            .0,
        "ide_extension_hash_mismatch"
    );
    assert!(
        server
            .seen()
            .iter()
            .any(|seen| seen.path.ends_with(".vsix")),
        "the VSIX was downloaded and checked"
    );
    assert_eq!(
        allowed(root.path()),
        json!({ BUILT_IN: [built_in_version()] })
    );
    assert!(
        listing(root.path(), &project, NOW + 1)
            .unwrap()
            .approved
            .is_empty()
    );
    assert_eq!(
        cache_files(root.path()),
        Vec::<String>::new(),
        "neither the download nor its partial file is kept"
    );
    assert_eq!(
        asks(root.path(), &project, NOW + 1).unwrap(),
        [AskView {
            id: ID.to_owned(),
            version: "1.0.0".to_owned(),
            current: None,
        }],
        "the ask still waits, so the owner sees it was not approved"
    );

    let pin = Approved {
        id: ID.to_owned(),
        version: "1.0.0".to_owned(),
        sha256: sha256(b"genuine bytes"),
        approved_ms: NOW,
    };
    let kept = cached(root.path(), ID, "1.0.0");
    cache_folder(root.path()).unwrap();
    std::fs::write(&kept, b"swapped on disk").unwrap();
    assert_eq!(
        verified(root.path(), &registry, &pin).unwrap_err().0,
        "ide_extension_hash_mismatch",
        "a cached VSIX that no longer matches its pin is not installed"
    );
    assert!(!kept.exists(), "the mismatched cached file is removed");
    assert_eq!(cache_files(root.path()), Vec::<String>::new());
    assert_eq!(std::fs::read_dir(&extensions).unwrap().count(), 0);
}

#[test]
fn install_request_outside_open_vsx_is_refused() {
    let open_vsx = Registry::open_vsx();
    assert_eq!(
        open_vsx.metadata(ID, "1.0.0").unwrap().as_str(),
        "https://open-vsx.org/api/acme/tool/1.0.0"
    );
    assert_eq!(
        open_vsx.metadata(ID, LATEST).unwrap().as_str(),
        "https://open-vsx.org/api/acme/tool"
    );
    assert!(
        open_vsx
            .inside("https://open-vsx.org/api/acme/tool/1.0.0/file/acme.tool-1.0.0.vsix")
            .is_ok()
    );
    for outside in [
        "https://evil.example/api/acme/tool/1.0.0/file/acme.tool-1.0.0.vsix",
        "http://open-vsx.org/api/acme/tool/1.0.0/file/acme.tool-1.0.0.vsix",
        "https://open-vsx.org:8443/api/acme/tool/1.0.0/file/acme.tool-1.0.0.vsix",
        "https://open-vsx.org/elsewhere/acme.tool-1.0.0.vsix",
        "https://owner@open-vsx.org/api/acme/tool/1.0.0/file/acme.tool-1.0.0.vsix",
        "https://open-vsx.org/api/acme/tool/1.0.0/file/acme.tool-1.0.0.vsix?mirror=1",
        "https://open-vsx.org.evil.example/api/acme.tool-1.0.0.vsix",
        "file:///tmp/acme.tool-1.0.0.vsix",
    ] {
        assert_eq!(
            open_vsx.inside(outside).unwrap_err().0,
            "ide_extension_source_refused",
            "{outside}"
        );
    }

    let root = root();
    let project = project();
    let elsewhere = Release {
        version: "1.0.0",
        served: b"tool one".to_vec(),
        published: sha256(b"tool one"),
        download: Some("https://evil.example/acme.tool-1.0.0.vsix".to_owned()),
    };
    let (server, registry) = registry(vec![elsewhere], "1.0.0");
    ask(root.path(), &project, ID, "1.0.0", NOW).unwrap();
    assert_eq!(
        approve(root.path(), &registry, &project, ID, "1.0.0", NOW + 1)
            .unwrap_err()
            .0,
        "ide_extension_source_refused"
    );
    assert_eq!(
        server
            .seen()
            .iter()
            .map(|seen| seen.path.as_str())
            .collect::<Vec<_>>(),
        ["/api/acme/tool/1.0.0"],
        "nothing is fetched past the metadata"
    );
    assert!(
        listing(root.path(), &project, NOW + 1)
            .unwrap()
            .approved
            .is_empty()
    );
    assert_eq!(cache_files(root.path()), Vec::<String>::new());

    for (id, version) in [
        ("Acme.Tool", "1.0.0"),
        ("acme", "1.0.0"),
        ("acme.tool/x", "1.0.0"),
        ("acme.tool", "../1.0.0"),
        ("acme.tool", ".hidden"),
    ] {
        assert_eq!(
            ask(root.path(), &project, id, version, NOW).unwrap_err().0,
            "ide_extension_invalid",
            "{id} {version}"
        );
    }
}

#[test]
fn update_is_shown_and_not_installed_until_approved() {
    let root = root();
    let project = project();
    let (server, registry) = registry(
        vec![
            Release::genuine("1.0.0", b"tool one"),
            Release::genuine("1.1.0", b"tool one point one"),
        ],
        "1.1.0",
    );
    ask(root.path(), &project, ID, "1.0.0", NOW).unwrap();
    approve(root.path(), &registry, &project, ID, "1.0.0", NOW + 1).unwrap();

    assert_eq!(check_updates(root.path(), &registry, NOW + 2).unwrap(), 1);
    let shown = listing(root.path(), &project, NOW + 2).unwrap();
    assert_eq!(
        shown.updates,
        [AskView {
            id: ID.to_owned(),
            version: "1.1.0".to_owned(),
            current: Some("1.0.0".to_owned()),
        }]
    );
    assert_eq!(
        shown.approved,
        [Update {
            id: ID.to_owned(),
            version: "1.0.0".to_owned(),
        }],
        "an available update changes no pin"
    );
    assert_eq!(
        allowed(root.path()),
        json!({ ID: ["1.0.0"], BUILT_IN: [built_in_version()] })
    );
    assert_eq!(cache_files(root.path()), [format!("{ID}-1.0.0.vsix")]);

    let requests = server.seen().len();
    assert_eq!(
        check_updates(root.path(), &registry, NOW + 60 * 60 * 1000).unwrap(),
        1
    );
    assert_eq!(
        server.seen().len(),
        requests,
        "a second check within a day reads nothing"
    );
    assert!(!update_due(root.path(), NOW + UPDATE_EVERY_MS - 1));
    assert!(update_due(root.path(), NOW + 2 + UPDATE_EVERY_MS));

    assert_eq!(
        ask(root.path(), &project, ID, "1.1.0", NOW + 3).unwrap(),
        Asked::Confirm,
        "the IDE's update asks the owner"
    );
    assert_eq!(
        allowed(root.path()),
        json!({ ID: ["1.0.0"], BUILT_IN: [built_in_version()] }),
        "the update waits for the desktop confirm"
    );
    assert_eq!(
        asks(root.path(), &project, NOW + 3).unwrap(),
        [AskView {
            id: ID.to_owned(),
            version: "1.1.0".to_owned(),
            current: Some("1.0.0".to_owned()),
        }]
    );

    let pinned = approve(root.path(), &registry, &project, ID, "1.1.0", NOW + 4).unwrap();
    assert_eq!(pinned.sha256, sha256(b"tool one point one"));
    assert_eq!(
        allowed(root.path()),
        json!({ ID: ["1.1.0"], BUILT_IN: [built_in_version()] })
    );
    let after = listing(root.path(), &project, NOW + 4).unwrap();
    assert!(after.updates.is_empty());
    assert!(after.asks.is_empty());
    assert_eq!(cache_files(root.path()), [format!("{ID}-1.1.0.vsix")]);
}

/// DEC-098: the build grants Copilot Chat the default chat participant, which brings the Chat
/// view back whatever `chat.disableAIFeatures` says, so an ask or an approval of it is refused
/// before the network and an ask or pin an older engine kept no longer counts.
#[test]
fn copilot_chat_is_refused_at_ask_and_approval_and_an_old_pin_no_longer_counts() {
    let root = root();
    let project = project();
    let chat = "github.copilot-chat";
    let (server, registry) = registry(vec![Release::genuine("1.0.0", b"chat")], "1.0.0");
    assert_eq!(
        ask(root.path(), &project, chat, "1.0.0", NOW)
            .unwrap_err()
            .0,
        "ide_extension_ai_locked"
    );
    assert_eq!(
        approve(root.path(), &registry, &project, chat, "1.0.0", NOW)
            .unwrap_err()
            .0,
        "ide_extension_ai_locked"
    );
    assert!(
        server.seen().is_empty(),
        "a refusal never reaches the network"
    );

    let old = Policy {
        approved: vec![Approved {
            id: chat.to_owned(),
            version: "1.0.0".to_owned(),
            sha256: sha256(b"chat"),
            approved_ms: NOW,
        }],
        asks: vec![Ask {
            project: project.clone(),
            id: chat.to_owned(),
            version: "1.0.0".to_owned(),
            asked_ms: NOW,
        }],
        ..Policy::default()
    };
    save(root.path(), &old).unwrap();
    assert!(
        allowed(root.path()).get(chat).is_none(),
        "an old pin is not allowed"
    );
    assert!(
        asks(root.path(), &project, NOW).unwrap().is_empty(),
        "an old ask is not shown"
    );
    assert!(
        listing(root.path(), &project, NOW)
            .unwrap()
            .approved
            .is_empty()
    );
}

/// A pin of Copilot Chat an older engine saved is gone once the policy loads, and only that
/// pin: the listing and the allowed setting keep every other extension (DEC-098).
#[test]
fn an_old_pin_of_copilot_chat_is_dropped_when_the_policy_loads() {
    let root = root();
    let project = project();
    let pin = |id: &str| Approved {
        id: id.to_owned(),
        version: "1.0.0".to_owned(),
        sha256: sha256(id.as_bytes()),
        approved_ms: NOW,
    };
    let old = Policy {
        approved: vec![pin(AI_LOCKED), pin(ID)],
        ..Policy::default()
    };
    save(root.path(), &old).unwrap();
    assert_eq!(
        listing(root.path(), &project, NOW).unwrap().approved,
        [Update {
            id: ID.to_owned(),
            version: "1.0.0".to_owned(),
        }]
    );
    let allowed = allowed(root.path());
    assert!(allowed.get(AI_LOCKED).is_none(), "{allowed:?}");
    assert!(allowed.get(ID).is_some(), "{allowed:?}");
}

/// An update of Copilot Chat that an earlier check left waiting is gone once the policy
/// loads, and only that update, so the owner is never offered it (DEC-098).
#[test]
fn a_pending_update_of_copilot_chat_is_dropped_when_the_policy_loads() {
    let root = root();
    let project = project();
    let waiting = |id: &str| Update {
        id: id.to_owned(),
        version: "1.1.0".to_owned(),
    };
    let old = Policy {
        updates: vec![waiting(AI_LOCKED), waiting(ID)],
        ..Policy::default()
    };
    save(root.path(), &old).unwrap();
    let shown = listing(root.path(), &project, NOW).unwrap();
    let ids: Vec<&str> = shown
        .updates
        .iter()
        .map(|update| update.id.as_str())
        .collect();
    assert_eq!(ids, [ID]);
}

#[test]
fn an_approval_needs_the_ide_ask_and_a_decline_or_dismissal_drops_it() {
    let root = root();
    let project = project();
    let other = ProjectId::new("p2").unwrap();
    let (server, registry) = registry(vec![Release::genuine("1.0.0", b"tool one")], "1.0.0");
    assert_eq!(
        approve(root.path(), &registry, &project, ID, "1.0.0", NOW)
            .unwrap_err()
            .0,
        "ide_extension_ask_missing",
        "the owner approves only what the IDE asked"
    );
    assert!(server.seen().is_empty());

    ask(root.path(), &other, ID, "1.0.0", NOW).unwrap();
    assert!(asks(root.path(), &project, NOW).unwrap().is_empty());
    assert_eq!(
        approve(root.path(), &registry, &project, ID, "1.0.0", NOW)
            .unwrap_err()
            .0,
        "ide_extension_ask_missing",
        "another project's ask does not count"
    );
    assert_eq!(
        approve(
            root.path(),
            &registry,
            &other,
            ID,
            "1.0.0",
            NOW + ASK_TTL_MS
        )
        .unwrap_err()
        .0,
        "ide_extension_ask_missing",
        "an expired ask does not count"
    );

    ask(root.path(), &project, ID, "1.0.0", NOW).unwrap();
    decline(root.path(), &project, ID, "1.0.0").unwrap();
    assert!(asks(root.path(), &project, NOW).unwrap().is_empty());
    assert_eq!(asks(root.path(), &other, NOW).unwrap().len(), 1);
    dismiss(root.path(), &other, ID, "1.0.0").unwrap();
    assert!(asks(root.path(), &other, NOW).unwrap().is_empty());

    for n in 0..=ASKS_PER_PROJECT_MAX {
        ask(
            root.path(),
            &project,
            &format!("acme.tool{n}"),
            "1.0.0",
            NOW + i64::try_from(n).unwrap(),
        )
        .unwrap();
    }
    let kept = asks(root.path(), &project, NOW + 100).unwrap();
    assert_eq!(kept.len(), ASKS_PER_PROJECT_MAX);
    assert_eq!(kept[0].id, "acme.tool1", "a project keeps its newest asks");
    assert!(server.seen().is_empty(), "an ask never reaches the network");
}

/// The ids a product file gives the Chat view: its default chat extension and every extension
/// holding the `defaultChatParticipant` proposal, in lower case. Input: the parsed file.
/// Output: the set of ids.
fn chat_ids(product: &serde_json::Value) -> std::collections::BTreeSet<String> {
    let chat = product["defaultChatAgent"]["chatExtensionId"]
        .as_str()
        .unwrap()
        .to_lowercase();
    let granted = product["extensionEnabledApiProposals"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, proposals)| {
            proposals
                .as_array()
                .unwrap()
                .iter()
                .any(|proposal| proposal == "defaultChatParticipant")
        })
        .map(|(id, _)| id.to_lowercase());
    std::iter::once(chat).chain(granted).collect()
}

/// [`AI_LOCKED`] is one literal, so the pinned build's product file has to agree with it: the
/// only id it gives the Chat view is that one, and a product naming another id or granting
/// the proposal to another id fails this check (IDE-7-L1).
#[test]
fn ai_locked_is_the_only_chat_extension_of_the_pinned_product() {
    let path = crate::ide::server::tests::cached_build().join("product.json");
    let product: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let locked = std::collections::BTreeSet::from([AI_LOCKED.to_owned()]);
    assert_eq!(chat_ids(&product), locked);
    let mut named = product.clone();
    named["defaultChatAgent"]["chatExtensionId"] = serde_json::json!("Other.chat");
    assert_ne!(chat_ids(&named), locked);
    let mut granted = product.clone();
    granted["extensionEnabledApiProposals"]["Other.chat"] =
        serde_json::json!(["defaultChatParticipant"]);
    assert_ne!(chat_ids(&granted), locked);
}
