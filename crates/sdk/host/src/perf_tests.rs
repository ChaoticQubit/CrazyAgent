use std::hint::black_box;

use bus_gateway::{AllOn, Gateway, Grants, Refusals};
use sdk::generated::{
    editor, net, process, project_rows, sandbox, secrets, service, settings, update,
};
use sdk::{Capability, Refusal, Tier};

use crate::perf_kit::{Row, SAMPLES, time};
use crate::{Bound, Providers};

const BATCH: usize = 1000;
const PLUGIN: &str = "perf";
const PERMISSIONS: [&str; 12] = [
    "editor.spawn",
    "net.request",
    "process.install",
    "sandbox.settings_write",
    "sandbox.settings_read",
    "secrets.put",
    "secrets.find",
    "secrets.forget",
    "service.read",
    "service.write",
    "settings.write",
    "update.write",
];
const PROJECT: &str = "notes";
const URL: &str = "https://api.github.com/user";
const TOOL: &str = "ruff";
const VERSION: &str = "0.6.9";
const SECRET_NAME: &str = "plugin-key-openai";
const SECRET_ID: u64 = 7;
const PID: u32 = 4242;
const SETTINGS_VERSION: u64 = 3;
const QUIT: settings::QuitSettings = settings::QuitSettings {
    on_quit: settings::OnQuit::Stop,
    left_running: settings::LeftRunning::Never,
    hours: 12,
};
const SANDBOX: sandbox::SandboxSettings = sandbox::SandboxSettings {
    linux_sockets: sandbox::LinuxSockets::TcpOnly,
    linux_supervisor: sandbox::LinuxSupervisor::On,
    linux_strict: sandbox::LinuxStrict::Off,
};
const STATE: service::ServiceState = service::ServiceState {
    installed: true,
    running: true,
    current: true,
};

/// A provider for every namespace that answers at once with a fixed value, so a row holds
/// only the SDK path: the input bound check, the gateway admit, the dispatch to the
/// provider and the output bound check.
struct Noop;

impl editor::Provide for Noop {
    fn spawn(&self, _: &'static str, launch: editor::EditorLaunch) -> Result<u32, Refusal> {
        black_box(launch);
        Ok(PID)
    }
}

impl project_rows::Provide for Noop {
    fn page(
        &self,
        _: &'static str,
        after: Option<String>,
        limit: u16,
    ) -> Result<project_rows::RowsPage, Refusal> {
        black_box((after, limit));
        Ok(project_rows::RowsPage {
            rows: Vec::new(),
            more: false,
        })
    }
}

impl net::Provide for Noop {
    fn request(&self, _: &'static str, request: net::Request) -> Result<net::Reply, Refusal> {
        black_box(request);
        Ok(net::Reply {
            status: 200,
            body: Vec::new(),
            headers: Vec::new(),
            captured: Vec::new(),
        })
    }
}

impl process::Provide for Noop {
    fn install(
        &self,
        _: &'static str,
        recipe: process::InstallRecipe,
    ) -> Result<process::InstallResult, Refusal> {
        black_box(recipe);
        Ok(process::InstallResult {
            code: 0,
            output: Vec::new(),
        })
    }
}

impl sandbox::Provide for Noop {
    fn save_settings(
        &self,
        _: &'static str,
        settings: sandbox::SandboxSettings,
        version: u64,
    ) -> Result<(), Refusal> {
        black_box((settings, version));
        Ok(())
    }

    fn settings(&self, _: &'static str) -> Result<sandbox::StoredSandboxSettings, Refusal> {
        Ok(sandbox::StoredSandboxSettings {
            settings: SANDBOX,
            version: SETTINGS_VERSION,
        })
    }
}

impl secrets::Provide for Noop {
    fn put_from_owner(&self, _: &'static str, name: String) -> Result<secrets::Secret, Refusal> {
        black_box(name);
        Ok(secrets::Secret::from_kernel(SECRET_ID))
    }

    fn find(&self, _: &'static str, name: String) -> Result<Option<secrets::Secret>, Refusal> {
        black_box(name);
        Ok(Some(secrets::Secret::from_kernel(SECRET_ID)))
    }

    fn forget(&self, _: &'static str, secret: secrets::Secret) -> Result<(), Refusal> {
        black_box(secret);
        Ok(())
    }
}

impl service::Provide for Noop {
    fn status(&self, _: &'static str) -> Result<service::ServiceState, Refusal> {
        Ok(STATE)
    }

    fn repair(&self, _: &'static str) -> Result<(), Refusal> {
        Ok(())
    }

    fn install(&self, _: &'static str) -> Result<(), Refusal> {
        Ok(())
    }

    fn uninstall(&self, _: &'static str) -> Result<(), Refusal> {
        Ok(())
    }
}

impl settings::Provide for Noop {
    fn save_quit(&self, _: &'static str, settings: settings::QuitSettings) -> Result<(), Refusal> {
        black_box(settings);
        Ok(())
    }
}

impl update::Provide for Noop {
    fn set_channel(&self, _: &'static str, channel: update::UpdateChannel) -> Result<(), Refusal> {
        black_box(channel);
        Ok(())
    }
}

/// A refusal sink the rows never reach: the bound plugin holds every grant.
struct Unreached;

impl Refusals for Unreached {
    fn refused(&self, plugin: &'static str, capability: &Capability, _: &Refusal) {
        panic!("perf row refused: {plugin} {}", capability.name);
    }
}

fn row(name: &'static str) -> Row {
    Row {
        layer: 1,
        module: "sdk",
        name,
        batch: BATCH,
    }
}

fn launch() -> editor::EditorLaunch {
    editor::EditorLaunch {
        project: PROJECT.to_owned(),
    }
}

fn request() -> net::Request {
    net::Request {
        method: net::Method::Get,
        url: URL.to_owned(),
        headers: vec![net::Field {
            name: "accept".to_owned(),
            value: "application/json".to_owned(),
        }],
        bearer: Some(secrets::Secret::from_kernel(SECRET_ID)),
        form: Vec::new(),
        secret_form: Vec::new(),
        json: None,
        reply_headers: Vec::new(),
        capture: Vec::new(),
    }
}

fn recipe() -> process::InstallRecipe {
    process::InstallRecipe {
        installer: process::Installer::UvTool,
        name: TOOL.to_owned(),
        verb: Some(process::InstallVerb::Install),
        version: Some(VERSION.to_owned()),
        target: None,
        python: None,
        lock: None,
        sha256: None,
        manager: None,
        args: Vec::new(),
        flags: Vec::new(),
    }
}

/// Times each of the 14 capability calls through the generated host binding a plugin
/// holds, with a no-op provider, after checking each answer once. Owned inputs are built
/// inside the timed step, so a row with a text input also holds that input's copy.
/// Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn capability_calls() {
    let grants = Grants::new(&[(PLUGIN, Tier::T2, &PERMISSIONS)]).unwrap();
    let gateway = Gateway::new(&grants, &Unreached, &AllOn);
    let providers = Providers {
        editor: &Noop,
        net: &Noop,
        process: &Noop,
        project_rows: &Noop,
        sandbox: &Noop,
        secrets: &Noop,
        service: &Noop,
        settings: &Noop,
        update: &Noop,
    };
    let host = Bound::new(gateway.bind(PLUGIN), &providers);
    let secret = secrets::Secret::from_kernel(SECRET_ID);
    assert_eq!(editor::spawn(&host, launch()), Ok(PID));
    assert_eq!(
        net::request(&host, request()).map(|reply| reply.status),
        Ok(200)
    );
    assert_eq!(
        process::install(&host, recipe()).map(|done| done.code),
        Ok(0)
    );
    assert_eq!(
        sandbox::save_settings(&host, SANDBOX, SETTINGS_VERSION),
        Ok(())
    );
    assert_eq!(
        sandbox::settings(&host).map(|stored| stored.version),
        Ok(SETTINGS_VERSION)
    );
    assert!(secrets::put_from_owner(&host, SECRET_NAME.to_owned()) == Ok(secret));
    assert!(secrets::find(&host, SECRET_NAME.to_owned()) == Ok(Some(secret)));
    assert_eq!(secrets::forget(&host, secret), Ok(()));
    assert_eq!(service::status(&host), Ok(STATE));
    assert_eq!(service::repair(&host), Ok(()));
    assert_eq!(service::install(&host), Ok(()));
    assert_eq!(service::uninstall(&host), Ok(()));
    assert_eq!(settings::save_quit(&host, QUIT), Ok(()));
    assert_eq!(
        update::set_channel(&host, update::UpdateChannel::Stable),
        Ok(())
    );
    let host = black_box(&host);
    time(&row("bound.editor.spawn"), SAMPLES, || {
        let _ = black_box(editor::spawn(host, launch()));
    });
    time(&row("bound.net.request"), SAMPLES, || {
        let _ = black_box(net::request(host, request()));
    });
    time(&row("bound.process.install"), SAMPLES, || {
        let _ = black_box(process::install(host, recipe()));
    });
    time(&row("bound.sandbox.save_settings"), SAMPLES, || {
        let _ = black_box(sandbox::save_settings(
            host,
            black_box(SANDBOX),
            black_box(SETTINGS_VERSION),
        ));
    });
    time(&row("bound.sandbox.settings"), SAMPLES, || {
        let _ = black_box(sandbox::settings(host));
    });
    time(&row("bound.secrets.put_from_owner"), SAMPLES, || {
        let _ = black_box(secrets::put_from_owner(host, SECRET_NAME.to_owned()));
    });
    time(&row("bound.secrets.find"), SAMPLES, || {
        let _ = black_box(secrets::find(host, SECRET_NAME.to_owned()));
    });
    time(&row("bound.secrets.forget"), SAMPLES, || {
        let _ = black_box(secrets::forget(host, black_box(secret)));
    });
    time(&row("bound.service.status"), SAMPLES, || {
        let _ = black_box(service::status(host));
    });
    time(&row("bound.service.repair"), SAMPLES, || {
        let _ = black_box(service::repair(host));
    });
    time(&row("bound.service.install"), SAMPLES, || {
        let _ = black_box(service::install(host));
    });
    time(&row("bound.service.uninstall"), SAMPLES, || {
        let _ = black_box(service::uninstall(host));
    });
    time(&row("bound.settings.save_quit"), SAMPLES, || {
        let _ = black_box(settings::save_quit(host, black_box(QUIT)));
    });
    time(&row("bound.update.set_channel"), SAMPLES, || {
        let _ = black_box(update::set_channel(
            host,
            black_box(update::UpdateChannel::Stable),
        ));
    });
}
