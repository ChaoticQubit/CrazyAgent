/// How a limited-network agent's sockets are held on Linux, the owner's setting "Linux
/// limited-network sockets". Landlock limits outbound TCP by port only, so a UDP, ping or
/// other socket would reach any host. `TcpOnly` (the default) refuses every socket but TCP
/// and has names looked up over TCP, `Refuse` starts no limited-network agent, and
/// `TcpPorts` keeps only Landlock's TCP port rules. Other systems ignore it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxSockets {
    #[default]
    TcpOnly,
    Refuse,
    TcpPorts,
}

/// Whether a Linux agent runs under the call supervisor where this Linux holds it, the
/// owner's setting "Linux call supervisor" (DEC-179 addendum 2). `On` (the default) adds
/// the guards a launch needs: ports on this computer only, no new `.git` outside the
/// agent's own git folders, signals only to programs it started. `Off` keeps the launch
/// and every line as they were before the supervisor. Other systems ignore it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxSupervisor {
    #[default]
    On,
    Off,
}

/// Whether a Linux launch that does not hold the full Linux sandbox is refused, the owner's
/// setting "Refuse agents without the full Linux sandbox" (DEC-185). `Off` (the default)
/// starts it with what this Linux holds and the lines that say what it cannot; `On` refuses
/// it unless the engine finds the launch holds the full Linux sandbox. Team agents need the full sandbox whatever it says.
/// Other systems ignore it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxStrict {
    #[default]
    Off,
    On,
}

/// The owner's app-wide sandbox settings: how a limited-network agent's sockets are held
/// on Linux ("Linux limited-network sockets"), whether Linux agents run under the call
/// supervisor ("Linux call supervisor") and whether a Linux launch without the full sandbox
/// is refused (DEC-185). Before the first save every value is its default; a client that
/// sends no supervisor or strict choice gets the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxSettings {
    pub linux_sockets: LinuxSockets,
    #[serde(default)]
    pub linux_supervisor: LinuxSupervisor,
    #[serde(default)]
    pub linux_strict: LinuxStrict,
}

/// The owner's sandbox settings with the version of the read that showed them. A save
/// names that version, so a save from a window that read older settings is refused
/// `stale_revision` instead of turning a newer choice back (rv-sec2 F1). Fields: the
/// settings and the version, 0 before the first save.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSandboxSettings {
    pub settings: SandboxSettings,
    pub version: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh engine's three Linux options are TCP-only sockets, the call supervisor on and
    /// strict refusal off, the defaults the owner's settings screen shows before
    /// the first save.
    #[test]
    fn defaults_are_tcp_only_supervisor_on_strict_off() {
        let fresh = SandboxSettings::default();
        assert_eq!(fresh.linux_sockets, LinuxSockets::TcpOnly);
        assert_eq!(fresh.linux_supervisor, LinuxSupervisor::On);
        assert_eq!(fresh.linux_strict, LinuxStrict::Off);
        assert_eq!(StoredSandboxSettings::default().settings, fresh);
        assert_eq!(StoredSandboxSettings::default().version, 0);
    }

    /// The settings and stored settings serialize to the same bytes as 2.4.4, so a saved row,
    /// a settings log entry and the desktop client read the same words after the move, and a
    /// 2.4.4 client that sends only the sockets choice still reads, with an unknown key refused.
    #[test]
    fn the_stored_json_is_byte_equal_to_2_4_4() {
        let off = SandboxSettings {
            linux_supervisor: LinuxSupervisor::Off,
            ..SandboxSettings::default()
        };
        assert_eq!(
            serde_json::to_string(&off).unwrap(),
            r#"{"linux_sockets":"tcp_only","linux_supervisor":"off","linux_strict":"off"}"#
        );
        let stored = StoredSandboxSettings {
            settings: SandboxSettings {
                linux_sockets: LinuxSockets::TcpPorts,
                linux_supervisor: LinuxSupervisor::On,
                linux_strict: LinuxStrict::On,
            },
            version: 7,
        };
        let text = r#"{"settings":{"linux_sockets":"tcp_ports","linux_supervisor":"on","linux_strict":"on"},"version":7}"#;
        assert_eq!(serde_json::to_string(&stored).unwrap(), text);
        assert_eq!(
            serde_json::from_str::<StoredSandboxSettings>(text).unwrap(),
            stored
        );
        let old: SandboxSettings = serde_json::from_str(r#"{"linux_sockets":"refuse"}"#).unwrap();
        assert_eq!(old.linux_sockets, LinuxSockets::Refuse);
        assert_eq!(old.linux_supervisor, LinuxSupervisor::On);
        assert_eq!(old.linux_strict, LinuxStrict::Off);
        let unknown = r#"{"linux_sockets":"refuse","linux_extra":"on"}"#;
        assert!(serde_json::from_str::<SandboxSettings>(unknown).is_err());
    }
}
