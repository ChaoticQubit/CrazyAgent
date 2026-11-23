use std::path::PathBuf;

use kernel_common::Platform;
use kernel_processes::service::{Repair, Service, persistable};
use kernel_processes::update::Channel;
use sdk::Refusal;
use sdk::generated::{service, update};

use crate::paths::Layout;
use crate::service::HOOKS;

/// The failure code of a service step the kernel could not take, as the desktop names it.
const SERVICE_FAILED: &str = "engine_service_failed";

/// The engine's provider of `update.set_channel`: it saves the channel as
/// `crazyagent update --channel` does ([`crate::update::save_channel`]), so the file and its
/// failure codes are the CLI's.
pub struct UpdateProvider<'l> {
    pub layout: &'l Layout,
}

impl update::Provide for UpdateProvider<'_> {
    fn set_channel(&self, _: &'static str, channel: update::UpdateChannel) -> Result<(), Refusal> {
        let channel = match channel {
            update::UpdateChannel::Stable => Channel::Stable,
            update::UpdateChannel::Dev => Channel::Dev,
        };
        crate::update::save_channel(self.layout.root(), channel).map_err(|error| failed(error.code))
    }
}

/// The engine's provider of `service.*` for its own data folder. The caller is the running
/// engine, so no call stops, starts or waits for an engine: an install takes effect from the
/// next login, and a service that runs this engine is never removed under it.
pub struct ServiceProvider<'l> {
    pub layout: &'l Layout,
}

impl ServiceProvider<'_> {
    /// The login service of this engine's data folder, running this executable. Output: the
    /// service, or `engine_service_failed`.
    fn service(&self) -> Result<Service, Refusal> {
        let executable = std::env::current_exe().map_err(|_| failed(SERVICE_FAILED))?;
        crate::service::current_user(Platform::CURRENT, executable, self.layout)
            .map_err(|_| failed(SERVICE_FAILED))
    }

    /// Installs this engine's service definition when [`Service::repair`] says to, as the
    /// desktop's engine start does, without stopping or starting the engine. A definition
    /// that is fine as it is stays. Input: whether the owner asked for a reinstall. Output:
    /// Ok, `engine_app_not_installed`, `engine_newer_than_app` or `engine_service_failed`.
    fn install_when_due(&self, reinstall: bool) -> Result<(), Refusal> {
        let service = self.service()?;
        let installed = service
            .status()
            .map_err(|_| failed(SERVICE_FAILED))?
            .installed;
        let appdir = std::env::var_os("APPDIR").map(PathBuf::from);
        let bundled = service.executable.clone();
        let service = Service {
            executable: persistable(&bundled, self.layout.root(), appdir.as_deref(), &HOOKS)
                .map_err(|_| failed(SERVICE_FAILED))?,
            ..service
        };
        match service.repair(&bundled, installed, reinstall, &HOOKS) {
            Repair::Refuse => {
                service.refuse();
                Err(failed("engine_app_not_installed"))
            }
            Repair::Keep => Err(failed("engine_newer_than_app")),
            Repair::Install => service.install(&HOOKS).map_err(|_| failed(SERVICE_FAILED)),
            Repair::Start => Ok(()),
        }
    }
}

impl service::Provide for ServiceProvider<'_> {
    fn status(&self, _: &'static str) -> Result<service::ServiceState, Refusal> {
        let service = self.service()?;
        let status = service.status().map_err(|_| failed(SERVICE_FAILED))?;
        Ok(service::ServiceState {
            installed: status.installed,
            running: status.running,
            current: status.installed && !service.runs_another_program(),
        })
    }

    fn repair(&self, _: &'static str) -> Result<(), Refusal> {
        self.install_when_due(false)
    }

    fn install(&self, _: &'static str) -> Result<(), Refusal> {
        self.install_when_due(true)
    }

    fn uninstall(&self, _: &'static str) -> Result<(), Refusal> {
        let service = self.service()?;
        let status = service.status().map_err(|_| failed(SERVICE_FAILED))?;
        if status.running && !service.runs_another_program() {
            return Err(failed("service_runs_this_engine"));
        }
        service.uninstall().map_err(|_| failed(SERVICE_FAILED))
    }
}

/// A provider failure with a code. Input: the code. Output: the refusal.
fn failed(code: &str) -> Refusal {
    Refusal::Failed {
        code: code.to_owned(),
    }
}

#[cfg(test)]
mod tests;
