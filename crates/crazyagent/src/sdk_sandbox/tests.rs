use bus_gateway::{AllOn, Gateway, Grants};
use crazyagent_cap as cap;
use kernel_plugin_host::generated::permissions::BUNDLED_GRANTS;
use kernel_sandbox::settings::{LinuxSockets, LinuxStrict, LinuxSupervisor, SandboxSettings};
use rusqlite::Connection;
use sdk::generated::sandbox as generated;
use sdk::{Refusal, Tier};
use sdk_host::{Bound, Providers};

use super::SandboxProvider;
use crate::protocol::{Command, Outcome};
use crate::sdk_slice::EngineRefusals;
use crate::security_caps::Absent;
use crate::settings_log::tests::{moved, store};

/// The agent-quit plugin's id, from the engine's own table.
const QUIT: &str = BUNDLED_GRANTS[0].plugin;

/// A plugin a row's own grant table names, standing in for the Settings plugin.
const SETTINGS_ROW: &str = "settings-row";

/// The permission that saves the owner's sandbox settings.
const WRITE: &str = "sandbox.settings_write";

/// The permission that reads the owner's sandbox settings and their version.
const READ: &str = "sandbox.settings_read";

/// Every kernel sandbox settings value: each sockets, supervisor and strict choice.
/// Output: the twelve values.
fn every_settings() -> Vec<SandboxSettings> {
    let mut all = Vec::new();
    for linux_sockets in [
        LinuxSockets::TcpOnly,
        LinuxSockets::Refuse,
        LinuxSockets::TcpPorts,
    ] {
        for linux_supervisor in [LinuxSupervisor::On, LinuxSupervisor::Off] {
            for linux_strict in [LinuxStrict::Off, LinuxStrict::On] {
                all.push(SandboxSettings {
                    linux_sockets,
                    linux_supervisor,
                    linux_strict,
                });
            }
        }
    }
    assert_eq!(all.len(), 12);
    all
}

/// The settings a row saves: every choice away from its default.
fn changed() -> SandboxSettings {
    SandboxSettings {
        linux_sockets: LinuxSockets::Refuse,
        linux_supervisor: LinuxSupervisor::Off,
        linux_strict: LinuxStrict::On,
    }
}

/// Every logged event's kind and body, in seq order, none when the store has no log yet.
/// Input: the store. Output: the rows.
fn logged(db: &Connection) -> Vec<(String, Vec<u8>)> {
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
    db.prepare("SELECT kind, body FROM state_log ORDER BY seq")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The owner's stored sandbox settings and the version a save from this read names.
/// Input: the store. Output: the pair.
fn shown(db: &Connection) -> (SandboxSettings, u64) {
    match crate::sandbox_settings::read(db).unwrap() {
        crate::engine::owner::OwnerData::SandboxSettings { settings, version } => {
            (settings, version)
        }
        other => panic!("{other:?}"),
    }
}

/// Runs one host call as `plugin` with `permissions`, against the real sandbox provider
/// in the store's open transaction at its current revision. Inputs: the transaction, the
/// plugin, its permissions and the call. Output: the call's result and the refusals the
/// gateway logged for the plugin.
fn as_plugin<T>(
    tx: &rusqlite::Transaction<'_>,
    plugin: &'static str,
    permissions: &'static [&'static str],
    call: impl FnOnce(&Bound<'_, '_>) -> T,
) -> (T, u64) {
    let grants = Grants::new(&[(plugin, Tier::T2, permissions)]).unwrap();
    let refusals = EngineRefusals::new();
    let gateway = Gateway::new(&grants, &refusals, &AllOn);
    let provider = SandboxProvider {
        tx,
        revision: u64::try_from(moved(tx).0).unwrap(),
    };
    let providers = Providers {
        editor: &Absent,
        net: &Absent,
        process: &Absent,
        sandbox: &provider,
        secrets: &Absent,
        settings: &Absent,
        service: &Absent,
        update: &Absent,
    };
    let result = call(&Bound::new(gateway.bind(plugin), &providers));
    (result, refusals.count(plugin))
}

#[test]
fn the_cap_enums_equal_the_kernel_enums() {
    for kernel in every_settings() {
        let sdk = generated::SandboxSettings::from(kernel);
        assert_eq!(
            serde_json::to_vec(&sdk).unwrap(),
            serde_json::to_vec(&kernel).unwrap(),
            "{kernel:?}"
        );
        assert_eq!(SandboxSettings::from(sdk), kernel);
    }
    let file = cap::parse(include_str!("../../../sdk/caps/sandbox.cap")).unwrap();
    let save = file
        .capabilities
        .iter()
        .find(|capability| capability.name == "sandbox.save_settings")
        .unwrap();
    assert_eq!(save.emits.len(), 1);
    assert_eq!(save.emits[0].kind, crate::settings_log::KIND_SANDBOX);
    let cap_fields: Vec<&str> = save.emits[0]
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    let body = serde_json::to_value(crate::settings_log::Saved {
        settings: SandboxSettings::default(),
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
fn no_bundled_plugin_holds_a_sandbox_permission() {
    for grant in BUNDLED_GRANTS {
        for needs in [WRITE, READ] {
            assert!(
                !grant.permissions.contains(&needs),
                "{} {needs}",
                grant.plugin
            );
        }
    }
}

#[test]
fn agent_quit_is_refused_both_sandbox_capabilities_and_nothing_moves() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let before = moved(&tx);
    let (saved, saved_refusals) = as_plugin(&tx, QUIT, &["settings.write"], |host| {
        generated::save_settings(host, changed().into(), 0)
    });
    assert_eq!(
        saved,
        Err(Refusal::NotGranted {
            capability: "sandbox.save_settings",
            needs: WRITE,
        })
    );
    assert_eq!(saved_refusals, 1);
    let (read, read_refusals) = as_plugin(&tx, QUIT, &["settings.write"], generated::settings);
    assert_eq!(
        read,
        Err(Refusal::NotGranted {
            capability: "sandbox.settings",
            needs: READ,
        })
    );
    assert_eq!(read_refusals, 1);
    assert_eq!(moved(&tx), before);
    assert_eq!(shown(&tx), (SandboxSettings::default(), 0));
    assert!(logged(&tx).is_empty());
}

#[test]
fn the_save_grant_alone_cannot_read() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let (read, refusals) = as_plugin(&tx, SETTINGS_ROW, &[WRITE], generated::settings);
    assert_eq!(
        read,
        Err(Refusal::NotGranted {
            capability: "sandbox.settings",
            needs: READ,
        })
    );
    assert_eq!(refusals, 1);
}

#[test]
fn the_read_grant_alone_cannot_save_and_nothing_moves() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let before = moved(&tx);
    let (saved, refusals) = as_plugin(&tx, SETTINGS_ROW, &[READ], |host| {
        generated::save_settings(host, changed().into(), 0)
    });
    assert_eq!(
        saved,
        Err(Refusal::NotGranted {
            capability: "sandbox.save_settings",
            needs: WRITE,
        })
    );
    assert_eq!(refusals, 1);
    assert_eq!(moved(&tx), before);
    assert_eq!(shown(&tx), (SandboxSettings::default(), 0));
    assert!(logged(&tx).is_empty());
}

#[test]
fn the_read_grant_reads_the_stored_settings_and_their_version() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let (read, refusals) = as_plugin(&tx, SETTINGS_ROW, &[READ], generated::settings);
    assert_eq!(
        read,
        Ok(generated::StoredSandboxSettings {
            settings: SandboxSettings::default().into(),
            version: 0,
        })
    );
    assert_eq!(refusals, 0);
}

#[test]
fn a_granted_save_logs_the_event_the_owner_save_logs() {
    let mut plugin = store();
    let plugin_tx = plugin.transaction().unwrap();
    let before = moved(&plugin_tx);
    let (saved, refusals) = as_plugin(&plugin_tx, SETTINGS_ROW, &[WRITE], |host| {
        generated::save_settings(host, changed().into(), 0)
    });
    assert_eq!(saved, Ok(()));
    assert_eq!(refusals, 0);
    assert_eq!(moved(&plugin_tx).0, before.0 + 1);
    let mut owner = store();
    let owner_tx = owner.transaction().unwrap();
    let revision = u64::try_from(moved(&owner_tx).0).unwrap();
    let command = Command::OwnerSandbox {
        settings: changed(),
        version: 0,
    };
    assert_eq!(
        crate::settings_log::owner(&owner_tx, &command, revision).unwrap(),
        Outcome::Applied
    );
    assert_eq!(logged(&plugin_tx), logged(&owner_tx));
    assert_eq!(logged(&plugin_tx).len(), 1);
    assert_eq!(logged(&plugin_tx)[0].0, crate::settings_log::KIND_SANDBOX);
    assert_eq!(shown(&plugin_tx), shown(&owner_tx));
    assert_eq!(shown(&plugin_tx).0, changed());
}

#[test]
fn a_save_from_a_stale_read_is_refused_and_nothing_moves() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let both: &[&str] = &[WRITE, READ];
    let (read, _) = as_plugin(&tx, SETTINGS_ROW, both, generated::settings);
    let version = read.unwrap().version;
    let (first, _) = as_plugin(&tx, SETTINGS_ROW, both, |host| {
        generated::save_settings(host, changed().into(), version)
    });
    assert_eq!(first, Ok(()));
    let after_first = (moved(&tx), shown(&tx), logged(&tx));
    let (second, refusals) = as_plugin(&tx, SETTINGS_ROW, both, |host| {
        generated::save_settings(host, SandboxSettings::default().into(), version)
    });
    assert_eq!(
        second,
        Err(Refusal::Rejected {
            code: "stale_revision".to_owned(),
        })
    );
    assert_eq!(refusals, 0);
    assert_eq!((moved(&tx), shown(&tx), logged(&tx)), after_first);
    assert_eq!(after_first.1.0, changed());
}

#[test]
fn a_save_naming_the_largest_version_is_refused_and_nothing_moves() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let before = moved(&tx);
    let (saved, _) = as_plugin(&tx, SETTINGS_ROW, &[WRITE], |host| {
        generated::save_settings(host, changed().into(), u64::MAX)
    });
    assert!(saved.is_err(), "{saved:?}");
    assert_eq!(moved(&tx), before);
    assert_eq!(shown(&tx), (SandboxSettings::default(), 0));
    assert!(logged(&tx).is_empty());
}

#[test]
fn the_provider_saves_through_the_one_owner_save_path() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let provider = SandboxProvider {
        tx: &tx,
        revision: u64::try_from(moved(&tx).0).unwrap(),
    };
    assert_eq!(
        generated::Provide::save_settings(&provider, SETTINGS_ROW, changed().into(), 7),
        Err(Refusal::Rejected {
            code: "stale_revision".to_owned(),
        })
    );
    assert!(logged(&tx).is_empty());
}
