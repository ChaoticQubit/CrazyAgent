use bus_gateway::{AllOn, Grants, Refusals};
use crazyagent_cap as cap;
use crazyagent_plugin_agent_quit as agent_quit;
use crazyagent_plugin_check as plugin_check;
use sdk::settings::QuitSettings as SdkQuit;
use sdk::{Refusal, Tier};

use super::{EngineRefusals, Entry, REFUSALS_PENDING_MAX, bundled_grants, save_quit_with};
use crate::protocol::{Command, Outcome};
use crate::quit_settings::{LeftRunning, OnQuit, QuitSettings};
use crate::settings_log::tests::{moved, store};
use kernel_plugin_host::generated::permissions::BUNDLED_GRANTS;
use rusqlite::Connection;

/// The agent-quit plugin's id, from the engine's own table.
const QUIT: &str = BUNDLED_GRANTS[0].plugin;

/// The quit settings the rows save: stop, never, 12 hours.
fn stop_never_12() -> QuitSettings {
    QuitSettings {
        on_quit: OnQuit::Stop,
        left_running: LeftRunning::Never,
        hours: 12,
    }
}

/// Every logged event's aggregate, kind, schema and body, in seq order, none when the store
/// has no log yet. Input: the store. Output: the rows.
fn logged(db: &Connection) -> Vec<(String, String, i64, Vec<u8>)> {
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='state_log')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if !exists {
        return Vec::new();
    }
    db.prepare("SELECT aggregate, kind, schema, body FROM state_log ORDER BY seq")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn the_logged_quit_event_keeps_its_kind_schema_and_body() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let before = moved(&tx);
    let revision = u64::try_from(before.0).unwrap();
    let command = Command::OwnerAgentQuit {
        settings: stop_never_12(),
    };
    assert_eq!(
        crate::settings_log::owner(&tx, &command, revision).unwrap(),
        Outcome::Applied
    );
    assert_eq!(moved(&tx).0, before.0 + 1);
    assert_eq!(
        logged(&tx),
        vec![(
            "settings".to_owned(),
            "AgentQuitSaved".to_owned(),
            1,
            br#"{"settings":{"on_quit":"stop","left_running":"never","hours":12}}"#.to_vec(),
        )]
    );
}

/// Every engine quit settings value the rows compare: each choice pair at 1 and 168 hours.
fn every_quit() -> Vec<QuitSettings> {
    let mut all = Vec::new();
    for on_quit in [OnQuit::KeepRunning, OnQuit::Stop] {
        for left_running in [LeftRunning::AfterHours, LeftRunning::Never] {
            for hours in [1, 168] {
                all.push(QuitSettings {
                    on_quit,
                    left_running,
                    hours,
                });
            }
        }
    }
    all
}

/// Whether the store holds a quit settings row. Input: the store. Output: true when it does.
fn quit_row(db: &Connection) -> bool {
    db.query_row("SELECT COUNT(*) FROM agent_quit_settings", [], |row| {
        row.get::<_, i64>(0)
    })
    .unwrap()
        > 0
}

/// Saves stop, never, 12 through the slice with a grant table and a fresh sink, and asserts
/// the save is refused and moves nothing. Inputs: the grants and the sink. Output: none.
fn refused_moves_nothing(grants: &Grants, refusals: &EngineRefusals) {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let before = moved(&tx);
    let revision = u64::try_from(before.0).unwrap();
    let result = save_quit_with(
        agent_quit::save,
        grants,
        &AllOn,
        refusals,
        &tx,
        revision,
        &stop_never_12(),
    );
    assert_eq!(
        result.err().map(|error| error.0),
        Some("plugin_call_refused".to_owned())
    );
    assert_eq!(moved(&tx), before, "a refused save moves and logs nothing");
    assert!(!quit_row(&tx));
    assert!(logged(&tx).is_empty());
}

#[test]
fn the_quit_save_without_its_grant_is_refused_and_moves_nothing() {
    let grants = Grants::new(&[(QUIT, Tier::T2, &[])]).unwrap();
    let refusals = EngineRefusals::new();
    refused_moves_nothing(&grants, &refusals);
    assert_eq!(refusals.count(QUIT), 1);
}

#[test]
fn the_quit_save_of_an_unknown_plugin_is_refused_and_moves_nothing() {
    let grants = Grants::new(&[]).unwrap();
    let refusals = EngineRefusals::new();
    refused_moves_nothing(&grants, &refusals);
    assert_eq!(refusals.count(QUIT), 1);
}

#[test]
fn the_granted_quit_save_through_the_slice_logs_the_same_event() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let before = moved(&tx);
    let revision = u64::try_from(before.0).unwrap();
    let refusals = EngineRefusals::new();
    assert_eq!(
        save_quit_with(
            agent_quit::save,
            bundled_grants(),
            &AllOn,
            &refusals,
            &tx,
            revision,
            &stop_never_12()
        )
        .unwrap(),
        Outcome::Applied
    );
    assert_eq!(moved(&tx).0, before.0 + 1);
    assert!(quit_row(&tx));
    assert_eq!(logged(&tx).len(), 1);
    assert_eq!(refusals.count(QUIT), 0);
}

#[test]
fn the_sdk_and_engine_quit_bodies_are_equal() {
    for engine in every_quit() {
        let sdk = SdkQuit::from(engine);
        assert_eq!(
            serde_json::to_vec(&sdk).unwrap(),
            serde_json::to_vec(&engine).unwrap()
        );
        assert_eq!(QuitSettings::from(sdk), engine);
    }
    let file = cap::parse(include_str!("../../../sdk/caps/settings.cap")).unwrap();
    let capability = file
        .capabilities
        .iter()
        .find(|capability| capability.name == "settings.save_quit")
        .unwrap();
    assert_eq!(capability.emits.len(), 1);
    assert_eq!(capability.emits[0].kind, crate::settings_log::KIND_QUIT);
    let cap_fields: Vec<&str> = capability.emits[0]
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    let body = serde_json::to_value(crate::settings_log::Saved {
        settings: QuitSettings::default(),
    })
    .unwrap();
    let body_fields: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(cap_fields, body_fields);
}

#[test]
fn a_full_first_refusal_set_still_refuses_and_logs() {
    let refusals = EngineRefusals::new();
    let refusal = Refusal::NotGranted {
        capability: sdk::settings::SAVE_QUIT.name,
        needs: sdk::settings::SAVE_QUIT.needs,
    };
    for index in 0..REFUSALS_PENDING_MAX {
        let plugin: &'static str = Box::leak(format!("filler-{index}").into_boxed_str());
        refusals.refused(plugin, &sdk::settings::SAVE_QUIT, &refusal);
    }
    assert_eq!(refusals.pending_len(), REFUSALS_PENDING_MAX);
    let grants = Grants::new(&[]).unwrap();
    refused_moves_nothing(&grants, &refusals);
    assert_eq!(refusals.count(QUIT), 1);
    assert_eq!(refusals.pending_len(), REFUSALS_PENDING_MAX);
}

#[test]
fn the_engine_build_script_checks_every_plugin() {
    let script =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs"))
            .unwrap();
    assert!(script.contains("crazyagent_plugin_check::check_plugins("));
    assert!(script.contains("cargo:rerun-if-changed=../../plugins"));
    assert!(script.contains("cargo:rerun-if-changed=build.rs"));
}

#[test]
fn the_engine_depends_on_plugins_only_from_the_plugins_folder() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest: toml::Table = std::fs::read_to_string(root.join("Cargo.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let mut plugins = Vec::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(deps) = manifest.get(section).and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, dep) in deps {
            let path = dep.get("path").and_then(toml::Value::as_str);
            if name.starts_with("crazyagent-plugin-") && name != "crazyagent-plugin-check" {
                let path = path.unwrap_or_else(|| panic!("{name} is a path dependency"));
                let parts: Vec<&str> = path.split('/').collect();
                assert_eq!(
                    parts.len(),
                    5,
                    "{name} sits at ../../plugins/<tier>/<plugin>"
                );
                assert_eq!(&parts[..3], ["..", "..", "plugins"], "{name}");
                assert!(["t1", "t2", "t3"].contains(&parts[3]), "{name}");
                plugins.push((parts[3].to_owned(), root.join(path)));
            }
            if let Some(path) = path {
                assert!(
                    !path.contains("plugins/") || name.starts_with("crazyagent-plugin-"),
                    "{name} reaches into plugins/"
                );
            }
        }
    }
    assert_eq!(plugins.len(), 2);
    for (tier, folder) in plugins {
        let manifest = plugin_check::read_manifest(&folder).unwrap();
        assert_eq!(manifest.tier, tier, "the folder tier is the manifest tier");
        assert!(
            BUNDLED_GRANTS
                .iter()
                .any(|grant| grant.plugin == manifest.plugin)
        );
    }
}

/// Runs one quit save through the slice with a test-double plugin entry and the bundled
/// grants, rolls the request back as the engine does on an error, and returns the result
/// code. Inputs: the entry, the grants and the sink. Output: the engine error code, or
/// "applied"; asserts the store moved and logged nothing after the rollback.
fn shape(entry: Entry, grants: &Grants, refusals: &EngineRefusals) -> String {
    let mut db = store();
    let before = moved(&db);
    let revision = u64::try_from(before.0).unwrap();
    let tx = db.transaction().unwrap();
    let result = save_quit_with(
        entry,
        grants,
        &AllOn,
        refusals,
        &tx,
        revision,
        &stop_never_12(),
    );
    tx.rollback().unwrap();
    assert_eq!(moved(&db), before, "the store's revision is unchanged");
    assert!(!quit_row(&db), "no agent_quit_settings change");
    assert!(
        logged(&db)
            .iter()
            .all(|(_, kind, ..)| kind != "AgentQuitSaved"),
        "no AgentQuitSaved event"
    );
    match result {
        Ok(outcome) => format!("{outcome:?}"),
        Err(error) => error.0,
    }
}

#[test]
fn ok_without_a_host_call_is_invalid_and_moves_nothing() {
    let refusals = EngineRefusals::new();
    let code = shape(|_, _| Ok(()), bundled_grants(), &refusals);
    assert_eq!(code, "plugin_call_invalid");
}

#[test]
fn two_host_calls_are_invalid_and_move_nothing() {
    let entries: [Entry; 2] = [
        |host, settings| {
            host.settings_save_quit(settings)?;
            host.settings_save_quit(settings)
        },
        |host, settings| {
            let _ = host.settings_save_quit(settings);
            let _ = host.settings_save_quit(settings);
            Ok(())
        },
    ];
    for entry in entries {
        let refusals = EngineRefusals::new();
        assert_eq!(
            shape(entry, bundled_grants(), &refusals),
            "plugin_call_invalid"
        );
    }
}

#[test]
fn a_made_up_rejected_or_failed_is_invalid() {
    let entries: [Entry; 2] = [
        |_, _| {
            Err(Refusal::Rejected {
                code: "made_up".to_owned(),
            })
        },
        |_, _| {
            Err(Refusal::Failed {
                code: "made_up".to_owned(),
            })
        },
    ];
    for entry in entries {
        let refusals = EngineRefusals::new();
        assert_eq!(
            shape(entry, bundled_grants(), &refusals),
            "plugin_call_invalid"
        );
    }
}

#[test]
fn a_made_up_not_granted_without_a_gateway_refusal_is_invalid() {
    let refusals = EngineRefusals::new();
    let entry: Entry = |_, _| {
        Err(Refusal::NotGranted {
            capability: sdk::settings::SAVE_QUIT.name,
            needs: sdk::settings::SAVE_QUIT.needs,
        })
    };
    assert_eq!(
        shape(entry, bundled_grants(), &refusals),
        "plugin_call_invalid"
    );
    assert_eq!(refusals.count(QUIT), 0);
    assert_eq!(refusals.pending_len(), 0, "no line queued");
}

#[test]
fn a_gateway_refusal_is_refused_whatever_the_plugin_returns() {
    let grants = Grants::new(&[(QUIT, Tier::T2, &[])]).unwrap();
    let entries: [Entry; 3] = [
        |host, settings| {
            let _ = host.settings_save_quit(settings);
            Ok(())
        },
        |host, settings| {
            let _ = host.settings_save_quit(settings);
            Err(Refusal::Failed {
                code: "made_up".to_owned(),
            })
        },
        |host, settings| {
            let _ = host.settings_save_quit(settings);
            let _ = host.settings_save_quit(settings);
            Err(Refusal::Rejected {
                code: "made_up".to_owned(),
            })
        },
    ];
    for entry in entries {
        let refusals = EngineRefusals::new();
        assert_eq!(shape(entry, &grants, &refusals), "plugin_call_refused");
        assert!(refusals.count(QUIT) >= 1);
    }
}

#[test]
fn every_generated_capability_may_be_named_in_a_refusal() {
    assert!(sdk::CAPABILITIES.contains(&sdk::settings::SAVE_QUIT));
    for capability in sdk::CAPABILITIES {
        let body = serde_json::json!({
            "plugin": QUIT,
            "capability": capability.name,
            "needs": capability.needs,
        });
        assert!(
            super::call_refused_closed(&serde_json::to_vec(&body).unwrap()),
            "{}",
            capability.name
        );
    }
}

/// The switch fake of the off rows: the agent-quit plugin reads as turned off.
struct QuitOff;

impl bus_gateway::Switches for QuitOff {
    fn is_on(&self, plugin: &str) -> bool {
        plugin != QUIT
    }
}

#[test]
fn the_quit_save_of_an_off_plugin_is_refused_and_queues_no_missing_permission() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let before = moved(&tx);
    let revision = u64::try_from(before.0).unwrap();
    let refusals = EngineRefusals::new();
    let result = save_quit_with(
        agent_quit::save,
        bundled_grants(),
        &QuitOff,
        &refusals,
        &tx,
        revision,
        &stop_never_12(),
    );
    assert_eq!(
        result.err().map(|error| error.0),
        Some("plugin_call_refused".to_owned())
    );
    assert_eq!(
        moved(&tx),
        before,
        "an off plugin's save moves and logs nothing"
    );
    assert!(!quit_row(&tx));
    assert!(logged(&tx).is_empty());
    assert_eq!(refusals.count(QUIT), 1);
    assert_eq!(refusals.pending_len(), 0);
}
