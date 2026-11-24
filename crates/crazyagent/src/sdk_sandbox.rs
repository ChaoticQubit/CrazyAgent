use kernel_sandbox::settings::{LinuxSockets, LinuxStrict, LinuxSupervisor, SandboxSettings};
use rusqlite::Transaction;
use sdk::Refusal;
use sdk::generated::sandbox as generated;

use crate::engine::owner::OwnerData;
use crate::protocol::{Command, Outcome};

#[cfg(test)]
mod tests;

/// The engine's provider of `sandbox.*` inside one owner request: a save is the owner's
/// sandbox save command, so it passes the same version check and logs the same event, and
/// a read is the owner read with the version a save from it names.
pub struct SandboxProvider<'t, 'c> {
    pub tx: &'t Transaction<'c>,
    pub revision: u64,
}

impl generated::Provide for SandboxProvider<'_, '_> {
    fn save_settings(
        &self,
        _: &'static str,
        settings: generated::SandboxSettings,
        version: u64,
    ) -> Result<(), Refusal> {
        let command = Command::OwnerSandbox {
            settings: kernel_settings(settings),
            version,
        };
        match crate::settings_log::owner(self.tx, &command, self.revision) {
            Ok(Outcome::Applied) => Ok(()),
            Ok(Outcome::Rejected { code }) => Err(Refusal::Rejected { code }),
            Ok(_) => Err(Refusal::Failed {
                code: "plugin_call_invalid".to_owned(),
            }),
            Err(error) => Err(Refusal::Failed { code: error.0 }),
        }
    }

    fn settings(&self, _: &'static str) -> Result<generated::StoredSandboxSettings, Refusal> {
        match crate::sandbox_settings::read(self.tx) {
            Ok(OwnerData::SandboxSettings { settings, version }) => {
                Ok(generated::StoredSandboxSettings {
                    settings: sdk_settings(settings),
                    version,
                })
            }
            Ok(_) => Err(Refusal::Failed {
                code: "plugin_call_invalid".to_owned(),
            }),
            Err(error) => Err(Refusal::Failed { code: error.0 }),
        }
    }
}

/// The SDK form of the kernel sandbox settings, choice for choice. Input: the kernel
/// settings. Output: the same settings as the generated type.
fn sdk_settings(settings: SandboxSettings) -> generated::SandboxSettings {
    generated::SandboxSettings {
        linux_sockets: match settings.linux_sockets {
            LinuxSockets::TcpOnly => generated::LinuxSockets::TcpOnly,
            LinuxSockets::Refuse => generated::LinuxSockets::Refuse,
            LinuxSockets::TcpPorts => generated::LinuxSockets::TcpPorts,
        },
        linux_supervisor: match settings.linux_supervisor {
            LinuxSupervisor::On => generated::LinuxSupervisor::On,
            LinuxSupervisor::Off => generated::LinuxSupervisor::Off,
        },
        linux_strict: match settings.linux_strict {
            LinuxStrict::Off => generated::LinuxStrict::Off,
            LinuxStrict::On => generated::LinuxStrict::On,
        },
    }
}

/// The kernel form of a plugin's sandbox settings, choice for choice. Input: the generated
/// settings. Output: the same settings as the kernel type.
fn kernel_settings(settings: generated::SandboxSettings) -> SandboxSettings {
    SandboxSettings {
        linux_sockets: match settings.linux_sockets {
            generated::LinuxSockets::TcpOnly => LinuxSockets::TcpOnly,
            generated::LinuxSockets::Refuse => LinuxSockets::Refuse,
            generated::LinuxSockets::TcpPorts => LinuxSockets::TcpPorts,
        },
        linux_supervisor: match settings.linux_supervisor {
            generated::LinuxSupervisor::On => LinuxSupervisor::On,
            generated::LinuxSupervisor::Off => LinuxSupervisor::Off,
        },
        linux_strict: match settings.linux_strict {
            generated::LinuxStrict::Off => LinuxStrict::Off,
            generated::LinuxStrict::On => LinuxStrict::On,
        },
    }
}
