use crate::settings::LinuxSupervisor;

/// What this Linux holds for an agent beyond its Landlock file and network rules: `nested`
/// when it starts agents in a process namespace of their own, `scoped` when the kernel
/// keeps their signals inside their own Landlock domain (Linux 6.12 or newer),
/// `supervised` when the call supervisor holds every guard here, as launches start it,
/// `root` when CrazyAgent runs as root, where it starts no supervisor (rv-sec3 N1),
/// `armored` when AppArmor's user namespace restriction is why it nests none (SBX-3), and
/// `routed` when it gives an agent limited to ports on any host a network of its own (SBX-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxHost {
    pub nested: bool,
    pub scoped: bool,
    pub supervised: bool,
    pub root: bool,
    pub armored: bool,
    pub routed: bool,
}

impl LinuxHost {
    /// What this Linux holds under the owner's Linux call supervisor setting: supervised
    /// only while it is On, and run as root only while it is not Off, so the root line says
    /// why no supervisor runs. Input: the setting, none when it cannot be read, which shows
    /// every line. Output: the facts the access lines read; what is kept is never changed.
    pub fn under(self, calls: Option<LinuxSupervisor>) -> Self {
        assert!(!(self.root && self.supervised));
        let held = Self {
            supervised: self.supervised && calls == Some(LinuxSupervisor::On),
            root: self.root && calls != Some(LinuxSupervisor::Off),
            ..self
        };
        assert!(self.supervised || !held.supervised);
        assert!(!(held.root && held.supervised));
        held
    }
}
