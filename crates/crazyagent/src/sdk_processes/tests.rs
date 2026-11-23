use bus_gateway::{AllOn, Gateway, Grants};
use kernel_plugin_host::generated::permissions::BUNDLED_GRANTS;
use kernel_processes::update::Channel;
use sdk::generated::service::{Provide as _, ServiceState};
use sdk::generated::update::UpdateChannel;
use sdk::{Refusal, Tier};
use sdk_host::{Bound, Providers};

use super::{ServiceProvider, UpdateProvider};
use crate::sdk_slice::EngineRefusals;
use crate::security_caps::Absent;

/// The agent-quit plugin's id, from the engine's own table.
const QUIT: &str = BUNDLED_GRANTS[0].plugin;

/// A plugin a row's own grant table names, standing in for the Settings plugin.
const SETTINGS_ROW: &str = "settings-row";

/// The permissions the update and service capabilities need.
const NEEDS: [&str; 3] = ["update.write", "service.read", "service.write"];

/// A data folder the engine could use, made private as the engine makes it. Output: the
/// guard and its layout.
fn data_root() -> (tempfile::TempDir, crate::paths::Layout) {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().canonicalize().unwrap().join("data");
    crate::paths::private_folder(&root).unwrap();
    (folder, crate::paths::Layout::new(root).unwrap())
}

#[test]
fn no_bundled_plugin_holds_the_update_or_service_permissions() {
    for grant in BUNDLED_GRANTS {
        for needs in NEEDS {
            assert!(
                !grant.permissions.contains(&needs),
                "{} {needs}",
                grant.plugin
            );
        }
    }
}

#[test]
fn agent_quit_calling_each_update_and_service_capability_is_refused_once_each() {
    let (_folder, layout) = data_root();
    let update = UpdateProvider { layout: &layout };
    let service = ServiceProvider { layout: &layout };
    let refusals = EngineRefusals::new();
    let gateway = Gateway::new(crate::sdk_slice::bundled_grants(), &refusals, &AllOn);
    let providers = Providers {
        net: &Absent,
        process: &Absent,
        secrets: &Absent,
        settings: &Absent,
        service: &service,
        update: &update,
    };
    let host = Bound::new(gateway.bind(QUIT), &providers);
    let refused = |result: Result<(), Refusal>, capability: &'static str, needs: &'static str| {
        assert_eq!(
            result,
            Err(Refusal::NotGranted { capability, needs }),
            "{capability}"
        );
    };
    refused(
        sdk::Host::update_set_channel(&host, UpdateChannel::Dev),
        "update.set_channel",
        "update.write",
    );
    refused(
        sdk::Host::service_status(&host).map(|_| ()),
        "service.status",
        "service.read",
    );
    refused(
        sdk::Host::service_repair(&host),
        "service.repair",
        "service.write",
    );
    refused(
        sdk::Host::service_install(&host),
        "service.install",
        "service.write",
    );
    refused(
        sdk::Host::service_uninstall(&host),
        "service.uninstall",
        "service.write",
    );
    assert_eq!(refusals.count(QUIT), 5);
    assert!(!layout.root().join("update.toml").exists());
}

#[test]
fn a_test_grant_reaches_the_update_provider() {
    let (_folder, layout) = data_root();
    let update = UpdateProvider { layout: &layout };
    let grants = Grants::new(&[(SETTINGS_ROW, Tier::T2, &["update.write"])]).unwrap();
    let refusals = EngineRefusals::new();
    let gateway = Gateway::new(&grants, &refusals, &AllOn);
    let providers = Providers {
        net: &Absent,
        process: &Absent,
        secrets: &Absent,
        settings: &Absent,
        service: &Absent,
        update: &update,
    };
    let host = Bound::new(gateway.bind(SETTINGS_ROW), &providers);
    assert_eq!(
        sdk::Host::update_set_channel(&host, UpdateChannel::Dev),
        Ok(())
    );
    assert_eq!(refusals.count(SETTINGS_ROW), 0);
    assert_eq!(
        kernel_processes::update::read_channel(layout.root()).unwrap(),
        Channel::Dev
    );
}

#[test]
fn set_channel_writes_the_file_the_cli_writes() {
    let (_plugin, plugin) = data_root();
    let (_cli, cli) = data_root();
    for (channel, kernel) in [
        (UpdateChannel::Dev, Channel::Dev),
        (UpdateChannel::Stable, Channel::Stable),
    ] {
        sdk::generated::update::Provide::set_channel(
            &UpdateProvider { layout: &plugin },
            SETTINGS_ROW,
            channel,
        )
        .unwrap();
        crate::update::save_channel(cli.root(), kernel).unwrap();
        assert_eq!(
            std::fs::read(plugin.root().join("update.toml")).unwrap(),
            std::fs::read(cli.root().join("update.toml")).unwrap(),
            "{kernel:?}"
        );
    }
}

#[test]
fn service_status_matches_the_cli_status() {
    let (_folder, layout) = data_root();
    let executable = std::env::current_exe().unwrap();
    let service =
        crate::service::current_user(kernel_common::Platform::CURRENT, executable, &layout)
            .unwrap();
    let cli = service.status().unwrap();
    let state = ServiceProvider { layout: &layout }
        .status(SETTINGS_ROW)
        .unwrap();
    assert_eq!(
        state,
        ServiceState {
            installed: cli.installed,
            running: cli.running,
            current: cli.installed && !service.runs_another_program(),
        }
    );
}

#[test]
fn update_check_and_install_are_not_declared() {
    let cap = include_str!("../../../sdk/caps/update.cap");
    let declared: Vec<&str> = cap
        .lines()
        .filter(|line| line.starts_with("capability "))
        .collect();
    assert_eq!(
        declared,
        vec!["capability update.set_channel(channel: UpdateChannel) -> unit"]
    );
}
