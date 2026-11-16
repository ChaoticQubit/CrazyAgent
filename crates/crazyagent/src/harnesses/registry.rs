use super::manifest::{
    self, DriverKind, LimitsKind, Manifest, RecordKind, Refused, Role, SubAgents,
};
use super::{AccountKind, HarnessSpec};
use std::sync::{LazyLock, OnceLock};

/// The most manifests one registry holds.
pub const HARNESSES_MAX: usize = 64;

/// The manifests built into the app, in catalog order: the owner-named harnesses first, in
/// priority order. Each file is named after the id inside it.
const BUNDLED: [(&str, &str); 18] = [
    ("codex", include_str!("../../harness-manifests/codex.yaml")),
    (
        "claude-code",
        include_str!("../../harness-manifests/claude-code.yaml"),
    ),
    (
        "opencode",
        include_str!("../../harness-manifests/opencode.yaml"),
    ),
    (
        "gemini-cli",
        include_str!("../../harness-manifests/gemini-cli.yaml"),
    ),
    ("fx", include_str!("../../harness-manifests/fx.yaml")),
    (
        "openclaw",
        include_str!("../../harness-manifests/openclaw.yaml"),
    ),
    ("pi", include_str!("../../harness-manifests/pi.yaml")),
    ("rcli", include_str!("../../harness-manifests/rcli.yaml")),
    ("goose", include_str!("../../harness-manifests/goose.yaml")),
    ("aider", include_str!("../../harness-manifests/aider.yaml")),
    (
        "qwen-code",
        include_str!("../../harness-manifests/qwen-code.yaml"),
    ),
    ("crush", include_str!("../../harness-manifests/crush.yaml")),
    ("droid", include_str!("../../harness-manifests/droid.yaml")),
    (
        "copilot",
        include_str!("../../harness-manifests/copilot.yaml"),
    ),
    ("cline", include_str!("../../harness-manifests/cline.yaml")),
    (
        "continue",
        include_str!("../../harness-manifests/continue.yaml"),
    ),
    (
        "openhands",
        include_str!("../../harness-manifests/openhands.yaml"),
    ),
    ("kimi", include_str!("../../harness-manifests/kimi.yaml")),
];

/// One harness of the registry: its manifest and what the engine can do with it. `spec`
/// holds the catalog row once it has been built, so it is built at most once.
#[derive(Debug, Clone)]
pub struct Harness {
    manifest: Manifest,
    spec: OnceLock<HarnessSpec>,
}

impl Harness {
    /// The catalog id. Output: the id, as stored in team plans and connections.
    pub fn id(&'static self) -> &'static str {
        &self.manifest.id
    }

    /// The manifest this harness was read from. Output: the manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The other names a team plan or a connection may store for this harness.
    /// Output: the aliases, such as `claude` for Claude Code.
    pub fn aliases(&self) -> &[String] {
        &self.manifest.aliases
    }

    /// How the engine talks to it without a terminal. Output: the driver kind, or none for
    /// a harness with no piped mode the engine drives.
    pub fn piped(&self) -> Option<DriverKind> {
        self.manifest.piped
    }

    /// How the engine reads back a live terminal session. Output: the record reader kind,
    /// `Unavailable` when there is none.
    pub fn record(&self) -> RecordKind {
        self.manifest.record
    }

    /// Whether it can run a team agent. Output: the answer.
    pub fn runnable(&self) -> bool {
        self.manifest.roles.contains(&Role::Agent)
    }

    /// Whether it can run a project's master. Output: the answer.
    pub fn master(&self) -> bool {
        self.manifest.roles.contains(&Role::Master)
    }

    /// Whether it can run a research job. Output: the answer.
    pub fn research(&self) -> bool {
        self.manifest.roles.contains(&Role::Research)
    }

    /// How a lead on it gets sub-agents. Output: its own agent team, its own threads, or
    /// workers the app starts.
    pub fn sub_agents(&self) -> SubAgents {
        self.manifest.sub_agents
    }

    /// Whether the owner can switch its tools off. Output: false when the manifest says
    /// why the switch is not there.
    pub fn tools_off(&self) -> bool {
        self.manifest.tools_limited.is_none()
    }

    /// Whether a name, as a team plan or a connection stores it, is this harness.
    /// Input: the name. Output: true for its id or one of its aliases.
    pub fn named(&self, name: &str) -> bool {
        self.manifest.id == name || self.manifest.aliases.iter().any(|alias| alias == name)
    }

    /// The wire formats of the endpoint accounts it runs on, as the manifest lists them.
    /// Output: the names; one that ends in `*` stands for every format that starts with
    /// the text before it.
    pub fn account_protocols(&self) -> &[String] {
        &self.manifest.account_protocols
    }

    /// Whether it speaks one wire format of an endpoint account. Input: the format a
    /// connection stores, such as `openai-chat`. Output: the answer.
    pub fn speaks(&self, protocol: &str) -> bool {
        self.manifest
            .account_protocols
            .iter()
            .any(|listed| match listed.strip_suffix('*') {
                Some(start) => protocol.starts_with(start),
                None => listed == protocol,
            })
    }

    /// Whether it can run on one account (DEC-141 PA-4), the one rule every master, team
    /// agent and research job uses: a sign-in fits only the harness it was made with, and
    /// only one that lists subscription accounts; an endpoint account, with a key or
    /// without, fits every harness that speaks its wire format, whatever harness it was
    /// stored with. Input: the stored connection. Output: the answer.
    pub fn account_fits(&self, connection: &crate::config::ModelConnection) -> bool {
        use crate::config::{ModelAuthentication, ModelEndpoint};
        let protocol = connection.protocol.as_str();
        match (&connection.endpoint, &connection.auth) {
            (ModelEndpoint::Native {}, ModelAuthentication::NativeManaged { .. }) => {
                protocol == "native"
                    && self
                        .manifest
                        .account_kinds
                        .contains(&AccountKind::Subscription)
                    && self.named(connection.harness.as_str())
            }
            (
                ModelEndpoint::Http { .. },
                ModelAuthentication::ApiKey { .. } | ModelAuthentication::None {},
            ) => self.speaks(protocol),
            _ => false,
        }
    }

    /// The oldest version of the harness CrazyAgent can sign in with (DEC-166).
    /// Output: the version, or none for a harness CrazyAgent never signs in to.
    pub fn sign_in_minimum(&self) -> Option<&str> {
        self.manifest.sign_in_minimum.as_deref()
    }

    /// How the limits of a sign-in of this harness are read beyond its usage windows.
    /// Output: the reader, or none when they are not reported.
    pub fn limits(&self) -> Option<LimitsKind> {
        self.manifest.limits
    }

    /// The folders under the owner's home that hold this harness's own files.
    /// Output: the folders, relative to the home folder, none when the manifest names none.
    pub fn owner_folders(&self) -> &[String] {
        &self.manifest.owner_folders
    }

    /// The oldest version of the harness CrazyAgent drives. Output: the version, or none
    /// when the manifest sets no floor.
    pub fn minimum_version(&self) -> Option<&str> {
        self.manifest.minimum_version.as_deref()
    }

    /// The catalog row of this harness in the shape the rest of the engine reads, for
    /// `harnesses::specs()` only; everything else reads `harnesses::spec(id)` or `specs()`.
    /// The two name lists are copied into memory that lives as long as the process, because
    /// a spec holds plain text slices; the row is kept, so the copy is made once per harness
    /// however often this is called. Output: the spec.
    pub(super) fn spec(&'static self) -> HarnessSpec {
        let manifest = &self.manifest;
        let spec = *self.spec.get_or_init(|| HarnessSpec {
            id: &manifest.id,
            name: &manifest.name,
            binaries: kept(&manifest.binaries),
            locations: kept(&manifest.locations),
            headless: &manifest.headless,
            tools_limited: manifest.tools_limited.as_deref(),
            model_selection: manifest.model_selection,
            account_kinds: &manifest.account_kinds,
            launch: manifest.cannot_start.as_deref(),
        });
        assert_eq!(spec.binaries.len(), manifest.binaries.len());
        assert_eq!(spec.locations.len(), manifest.locations.len());
        spec
    }
}

fn kept(texts: &'static [String]) -> &'static [&'static str] {
    assert!(texts.len() <= manifest::LIST_MAX);
    let names: Vec<&'static str> = texts.iter().map(String::as_str).collect();
    Vec::leak(names)
}

/// Every harness the engine knows, in catalog order.
#[derive(Debug, Clone)]
pub struct Registry {
    harnesses: Vec<Harness>,
}

impl Registry {
    /// Builds a registry from manifest files, in the order given. A file must be named
    /// after the id inside it, and no id, alias or program name may be used twice.
    /// Input: each file's name without `.yaml` and its text. Output: the registry, or the
    /// first refusal with the file named in its sentence.
    pub fn build(files: &[(&str, &str)]) -> Result<Self, Refused> {
        assert!(files.len() <= HARNESSES_MAX);
        let mut harnesses: Vec<Harness> = Vec::with_capacity(files.len());
        for (file, text) in files {
            let manifest = manifest::parse(text.as_bytes()).map_err(|refused| Refused {
                code: refused.code,
                message: format!("{file}: {}", refused.message),
            })?;
            if manifest.id != *file {
                return Err(Refused {
                    code: manifest::VALUE,
                    message: format!(
                        "{file}: the id inside is {}; a manifest file is named after its id.",
                        manifest.id
                    ),
                });
            }
            super::drivers::acp::check(&manifest)
                .and_then(|()| super::accounts::check(&manifest))
                .and_then(|()| crate::skills::import::check(&manifest))
                .map_err(|refused| Refused {
                    code: refused.code,
                    message: format!("{file}: {}", refused.message),
                })?;
            for known in &harnesses {
                if let Some(message) = clash(&manifest, &known.manifest) {
                    return Err(Refused {
                        code: manifest::DUPLICATE,
                        message: format!("{file}: {message}"),
                    });
                }
            }
            harnesses.push(Harness {
                manifest,
                spec: OnceLock::new(),
            });
        }
        assert_eq!(harnesses.len(), files.len());
        Ok(Self { harnesses })
    }

    /// Where a harness sits in catalog order. Input: its id or one of its aliases.
    /// Output: the position, or none for a name no manifest has.
    pub fn position(&self, name: &str) -> Option<usize> {
        self.harnesses
            .iter()
            .position(|harness| harness.named(name))
    }

    /// Finds a harness. Input: its id or one of its aliases. Output: the harness, or none
    /// for a name no manifest has; a test build also finds the stand-in harnesses
    /// (`registry_standins.rs`), which no registry lists.
    pub fn get(&self, name: &str) -> Option<&Harness> {
        let found = self.position(name).map(|index| &self.harnesses[index]);
        #[cfg(test)]
        let found = found.or_else(|| standins::get(name));
        found
    }

    /// Every harness, in catalog order. Output: the harnesses.
    pub fn all(&self) -> &[Harness] {
        &self.harnesses
    }
}

fn clash(new: &Manifest, known: &Manifest) -> Option<String> {
    let names = |manifest: &Manifest| -> Vec<String> {
        std::iter::once(manifest.id.clone())
            .chain(manifest.aliases.iter().cloned())
            .collect()
    };
    let taken = names(known);
    if let Some(name) = names(new).into_iter().find(|name| taken.contains(name)) {
        return Some(format!("the name {name} is already used by {}.", known.id));
    }
    new.binaries
        .iter()
        .find(|binary| known.binaries.contains(binary))
        .map(|binary| format!("the program {binary} is already listed by {}.", known.id))
}

/// The registry of the manifests built into the app, read once. A bundled manifest that
/// does not load is a programming error. Output: the registry.
pub fn registry() -> &'static Registry {
    static BUILT: LazyLock<Registry> = LazyLock::new(|| {
        let built = Registry::build(&BUNDLED).expect("every bundled harness manifest loads");
        assert_eq!(built.harnesses.len(), BUNDLED.len());
        built
    });
    &BUILT
}

#[cfg(test)]
#[path = "registry_standins.rs"]
pub(crate) mod standins;

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
