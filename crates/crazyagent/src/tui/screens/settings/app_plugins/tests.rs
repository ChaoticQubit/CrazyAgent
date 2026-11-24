use super::*;
use crate::accounts::{AccountStatus, Provider};
use crate::engine::owner::transport::app_plugins::{PermissionWords, PluginChanged};
use crate::engine::owner::{APP_PLUGINS_CODES, WhenTurnedOff};
use crate::protocol::CredentialRef;
use crate::tui::client::OwnerEvent;
use crate::tui::frame::{HINT_ROWS_MAX, wrapped_footer};
use crate::tui::screens::open;
use crate::tui::screens::settings_accounts::tests::{check, ctx, render};
use crate::tui::seam::Request;
use crate::tui::shell::{Shell, targets};
use kernel_plugin_host::grants::GrantedCapability;
use kernel_plugin_host::views::Listing;

const SHA: &str = "5f2c0d8e9a7b6c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e4f3a2b1c0d";
const OLD_SHA: &str = "0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9";
const DOCK_TWO: &str = "/Users/owner/plugins/dock-two";
const SYNTHETIC_KEY: &str = "sk-test-tui-secret-77";
const WIDE: (u16, u16) = (100, 60);

/// A stand-in engine for the App plugins page: the listing, the grants, the permission
/// words, the notices the next read drains, the folders it accepts, the review it shows,
/// the runs holding a plugin, the stored turn-off choice, the accounts it lists, a
/// refusal for the next change or grants read, and the name of every job it was sent.
struct Fake {
    listing: Listing,
    grants: Vec<PluginGrants>,
    words: Vec<PermissionWords>,
    notices: Vec<PluginChanged>,
    folders: Vec<(PathBuf, Preview)>,
    review: Preview,
    users: Vec<PluginUser>,
    stored: TurnOff,
    project_override: bool,
    accounts: Vec<AccountView>,
    refuse: Option<String>,
    refuse_grants: Option<String>,
    seen: Vec<String>,
}

fn problem(code: &str, line: Option<u32>, field: Option<&str>, message: &str) -> Problem {
    Problem {
        code: code.into(),
        line,
        field: field.map(Into::into),
        message: message.into(),
    }
}

fn http(url: &str, hosts: &[&str], local: bool) -> RunsView {
    RunsView::Http {
        url: url.into(),
        hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
        local_unencrypted: local,
        dimensions: 256,
    }
}

fn plugin(name: &str, title: &str, origin: Origin, status: PluginStatus, on: bool) -> PluginView {
    PluginView {
        name: name.into(),
        title: title.into(),
        version: "1".into(),
        description: String::new(),
        kind: PluginKind::Embedding,
        origin,
        status,
        enabled: on,
        runs: None,
        secrets: Vec::new(),
        sha256: SHA.into(),
    }
}

fn secret(binding: Binding) -> Vec<SecretView> {
    vec![SecretView {
        name: "api_key".into(),
        binding,
    }]
}

/// The six plugins the listing carries: two bundled embedding plugins, one approved plugin
/// of the owner's with a key it entered, one never approved, one changed since approved,
/// and one refused.
fn listing() -> Listing {
    let mut openai = plugin(
        "openai",
        "OpenAI embeddings",
        Origin::Bundled,
        PluginStatus::NeedsKey,
        false,
    );
    openai.runs = Some(http(
        "https://api.openai.com/v1/embeddings",
        &["api.openai.com"],
        false,
    ));
    openai.secrets = secret(Binding::None);
    let mut ollama = plugin(
        "ollama",
        "Ollama embeddings",
        Origin::Bundled,
        PluginStatus::Ready,
        true,
    );
    ollama.runs = Some(http(
        "http://127.0.0.1:11434/api/embed",
        &["127.0.0.1"],
        true,
    ));
    let mut dock = plugin(
        "dock-embed",
        "Dock embeddings",
        Origin::User,
        PluginStatus::Ready,
        true,
    );
    dock.runs = Some(http("http://127.0.0.1:8080/embed", &["127.0.0.1"], true));
    dock.secrets = secret(Binding::Key);
    let mut harbor = plugin(
        "harbor",
        "Harbor embeddings",
        Origin::User,
        PluginStatus::NeedsReview,
        false,
    );
    harbor.runs = Some(http(
        "https://api.harbor.invalid/v1/embed",
        &["api.harbor.invalid"],
        false,
    ));
    harbor.secrets = secret(Binding::Account {
        account: "acct-002".into(),
        name: "Work OpenAI".into(),
    });
    let mut cove = plugin(
        "cove",
        "Cove embeddings",
        Origin::User,
        PluginStatus::Changed {
            trusted_sha256: OLD_SHA.into(),
        },
        false,
    );
    cove.runs = Some(http(
        "https://api.cove.invalid/v1/embed",
        &["api.cove.invalid"],
        false,
    ));
    let broken = plugin(
        "broken",
        "Broken plugin",
        Origin::User,
        PluginStatus::Refused {
            problem: problem(
                "app_plugin_host_undeclared",
                Some(7),
                Some("http.url"),
                "the URL's host is not in hosts.",
            ),
        },
        false,
    );
    Listing {
        plugins: vec![openai, ollama, dock, harbor, cove, broken],
        more: false,
    }
}

fn grant(plugin: &str, origin: Origin, essential: bool, capabilities: &[&str]) -> PluginGrants {
    PluginGrants {
        plugin: plugin.into(),
        tier: "t2".into(),
        origin,
        state: GrantState::On,
        essential,
        capabilities: capabilities
            .iter()
            .map(|name| GrantedCapability {
                name: (*name).into(),
                tier: "t2".into(),
            })
            .collect(),
    }
}

/// The grants read: two bundled code plugins, one essential, sorted by id among two
/// plugins the listing already carries, which the page must not list twice.
fn grants() -> Vec<PluginGrants> {
    vec![
        grant("agent-quit", Origin::Bundled, true, &["settings.save_quit"]),
        grant("dock-embed", Origin::User, false, &[]),
        grant("openai", Origin::Bundled, false, &[]),
        grant(
            "service-kit",
            Origin::Bundled,
            false,
            &["service.install", "service.status"],
        ),
    ]
}

fn words(name: &str, words: &str) -> PermissionWords {
    PermissionWords {
        name: name.into(),
        words: words.into(),
    }
}

fn trust(name: &str, title: &str, runs: RunsView) -> TrustView {
    TrustView {
        name: name.into(),
        title: title.into(),
        version: "2".into(),
        description: "Embeds on the dock machine.".into(),
        kind: PluginKind::Embedding,
        runs,
        secrets: Vec::new(),
        sha256: SHA.into(),
        replaces: false,
        changed: false,
        grew_hosts: Vec::new(),
    }
}

fn account(index: usize, name: &str, method: SignInMethod) -> AccountView {
    AccountView {
        id: CredentialRef::new(format!("acct-{index:03}")).unwrap(),
        name: name.into(),
        provider: Provider::OpenaiApi,
        provider_name: crate::accounts::provider_name(Provider::OpenaiApi).into(),
        method,
        method_label: String::new(),
        url: None,
        model: None,
        status: AccountStatus::KeySaved,
        status_label: AccountStatus::KeySaved.label().into(),
    }
}

impl Fake {
    fn new() -> Self {
        Self {
            listing: listing(),
            grants: grants(),
            words: vec![
                words(
                    "settings.save_quit",
                    "Save what the agents were doing when the app quits",
                ),
                words("service.install", "Install a background service"),
                words("service.status", "Read a background service's status"),
            ],
            notices: Vec::new(),
            folders: vec![(
                PathBuf::from(DOCK_TWO),
                Preview::Ready {
                    trust: trust(
                        "dock-two",
                        "Dock two",
                        http("http://127.0.0.1:9090/embed", &["127.0.0.1"], true),
                    ),
                },
            )],
            review: Preview::Ready {
                trust: trust(
                    "harbor",
                    "Harbor embeddings",
                    http(
                        "https://api.harbor.invalid/v1/embed",
                        &["api.harbor.invalid"],
                        false,
                    ),
                ),
            },
            users: vec![PluginUser {
                run_id: "run-1".into(),
                label: "Fix the login page".into(),
            }],
            stored: TurnOff::RefuseNewCalls,
            project_override: false,
            accounts: vec![
                account(1, "Claude Pro", SignInMethod::Subscription),
                account(2, "Work OpenAI", SignInMethod::ApiKey),
            ],
            refuse: None,
            refuse_grants: None,
            seen: Vec::new(),
        }
    }

    fn find(&mut self, name: &str) -> Option<&mut PluginView> {
        self.listing
            .plugins
            .iter_mut()
            .find(|plugin| plugin.name == name)
    }

    fn page(&mut self) -> AppPluginsEvent {
        if let Some(code) = self.refuse.take() {
            return AppPluginsEvent::Page(Err(code));
        }
        AppPluginsEvent::Page(Ok(PageReply {
            listing: self.listing.clone(),
            words: self.words.clone(),
            notices: std::mem::take(&mut self.notices),
            when_turned_off: WhenTurnedOff {
                value: self.stored,
                project_override: self.project_override,
            },
        }))
    }

    /// Answers one job the way the engine would, refusing an add or approval whose SHA-256
    /// is not the one the review showed. Input: the job. Output: the event.
    fn answer(&mut self, job: AppPluginsJob) -> AppPluginsEvent {
        self.seen.push(job.name().into());
        match job {
            AppPluginsJob::List => self.page(),
            AppPluginsJob::Grants => AppPluginsEvent::Grants(match self.refuse_grants.take() {
                Some(code) => Err(code),
                None => Ok(self.grants.clone()),
            }),
            AppPluginsJob::Preview { folder } => AppPluginsEvent::Preview(
                self.folders
                    .iter()
                    .find(|(known, _)| *known == folder)
                    .map(|(_, preview)| preview.clone())
                    .ok_or_else(|| "app_plugin_not_found".into()),
            ),
            AppPluginsJob::Review { .. } => AppPluginsEvent::Preview(Ok(self.review.clone())),
            AppPluginsJob::Add { folder, sha256, .. } => {
                assert_eq!(sha256, SHA, "the add carries the SHA-256 the review showed");
                assert_eq!(folder, PathBuf::from(DOCK_TWO));
                let mut added = plugin(
                    "dock-two",
                    "Dock two",
                    Origin::User,
                    PluginStatus::Ready,
                    true,
                );
                added.runs = Some(http("http://127.0.0.1:9090/embed", &["127.0.0.1"], true));
                self.listing.plugins.push(added);
                self.page()
            }
            AppPluginsJob::Trust { name, sha256 } => {
                assert_eq!(sha256, SHA);
                if let Some(plugin) = self.find(&name) {
                    plugin.status = PluginStatus::Ready;
                }
                self.page()
            }
            AppPluginsJob::SetEnabled { name, enabled, .. } => {
                if self.refuse.is_none() {
                    if let Some(plugin) = self.find(&name) {
                        plugin.enabled = enabled;
                    }
                    if let Some(grant) = self.grants.iter_mut().find(|one| one.plugin == name) {
                        grant.state = if enabled {
                            GrantState::On
                        } else {
                            GrantState::Off
                        };
                    }
                }
                self.page()
            }
            AppPluginsJob::Users { name } => AppPluginsEvent::Users {
                name,
                result: Ok(self.users.clone()),
            },
            AppPluginsJob::SetWhenOff { when_off } => {
                if self.refuse.is_none() {
                    self.stored = when_off;
                }
                self.page()
            }
            AppPluginsJob::Remove { name } => {
                self.listing.plugins.retain(|plugin| plugin.name != name);
                self.page()
            }
            AppPluginsJob::BindAccount {
                name,
                secret,
                account,
            } => {
                let label = self
                    .accounts
                    .iter()
                    .find(|one| one.id.as_str() == account)
                    .map(|one| one.name.clone())
                    .expect("an account the dialog listed");
                if let Some(plugin) = self.find(&name)
                    && let Some(one) = plugin.secrets.iter_mut().find(|one| one.name == secret)
                {
                    one.binding = Binding::Account {
                        account,
                        name: label,
                    };
                }
                self.page()
            }
            AppPluginsJob::SetKey { name, secret, .. } => {
                if self.refuse.is_none()
                    && let Some(plugin) = self.find(&name)
                    && let Some(one) = plugin.secrets.iter_mut().find(|one| one.name == secret)
                {
                    one.binding = Binding::Key;
                }
                self.page()
            }
            AppPluginsJob::Unbind { name, secret } => {
                if let Some(plugin) = self.find(&name)
                    && let Some(one) = plugin.secrets.iter_mut().find(|one| one.name == secret)
                {
                    one.binding = Binding::None;
                }
                self.page()
            }
        }
    }
}

fn requests(shell: &mut Shell) -> Vec<SettingsJob> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Settings(job) => Some(*job),
            _ => None,
        })
        .collect()
}

fn arrive(shell: &mut Shell, event: SettingsEvent) {
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        event,
    )))));
}

fn answer(shell: &mut Shell, fake: &mut Fake, job: AppPluginsJob) {
    let event = fake.answer(job);
    arrive(shell, SettingsEvent::AppPlugins(event));
}

/// Answers every request the page sent, and the ones those answers lead to.
fn serve(shell: &mut Shell, fake: &mut Fake) {
    for _ in 0..8 {
        let sent = requests(shell);
        if sent.is_empty() {
            return;
        }
        for job in sent {
            let event = match job {
                SettingsJob::AppPlugins(job) => SettingsEvent::AppPlugins(fake.answer(*job)),
                SettingsJob::AllAccounts => SettingsEvent::AllAccounts(Ok(fake.accounts.clone())),
                other => panic!("the page sent {other:?}"),
            };
            arrive(shell, event);
        }
    }
    panic!("the page kept asking");
}

fn one(shell: &mut Shell) -> AppPluginsJob {
    let mut sent = requests(shell);
    assert_eq!(sent.len(), 1, "one request at a time");
    match sent.remove(0) {
        SettingsJob::AppPlugins(job) => *job,
        other => panic!("expected an App plugins job, got {other:?}"),
    }
}

fn rows(shell: &mut Shell) -> Vec<String> {
    render(shell, WIDE.0, WIDE.1)
}

fn shows(shell: &mut Shell, text: &str) -> bool {
    rows(shell).iter().any(|row| row.contains(text))
}

fn line(shell: &mut Shell, title: &str) -> String {
    rows(shell)
        .into_iter()
        .find(|row| row.contains(title))
        .unwrap_or_default()
}

fn page(fake: &mut Fake) -> Shell {
    let mut shell = Shell::with(ctx(WIDE.0, WIDE.1), Box::new(AppPluginsPage::new()));
    serve(&mut shell, fake);
    shell
}

fn select(shell: &mut Shell, index: usize) {
    for _ in 0..12 {
        shell.key(Key::Up);
    }
    for _ in 0..index {
        shell.key(Key::Down);
    }
}

fn said(shell: &Shell) -> String {
    shell
        .ctx
        .message()
        .map(|message| format!("{} {}", message.lead, message.rest))
        .unwrap_or_default()
}

fn bar(shell: &mut Shell) -> String {
    let rows = rows(shell);
    crate::tui::screens::settings_accounts::tests::bar_rows(&rows).join("\n")
}

#[test]
fn the_page_reads_then_lists_each_plugin_with_one_label_and_code_plugins_last() {
    let mut fake = Fake::new();
    let mut shell = Shell::with(ctx(WIDE.0, WIDE.1), Box::new(AppPluginsPage::new()));
    assert!(matches!(one(&mut shell), AppPluginsJob::List));
    assert!(shows(&mut shell, "Reading app plugins..."));
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(AppPluginsEvent::Page(Err("app_plugins_unavailable".into()))),
    );
    assert!(shows(
        &mut shell,
        "App plugins aren't available in this engine yet."
    ));
    assert!(bar(&mut shell).contains("r Try again"));
    shell.key(Key::Char('r'));
    serve(&mut shell, &mut fake);
    assert_eq!(fake.seen, ["AppPlugins", "PluginGrants"]);

    assert!(line(&mut shell, "OpenAI embeddings").contains("Bundled  Off"));
    assert!(line(&mut shell, "Ollama embeddings").contains("Bundled  On"));
    assert!(line(&mut shell, "Dock embeddings").contains("Approved  On"));
    assert!(line(&mut shell, "Harbor embeddings").contains("Not approved"));
    assert!(line(&mut shell, "Cove embeddings").contains("Changed - review"));
    assert!(line(&mut shell, "Broken plugin").contains("Refused"));
    assert!(line(&mut shell, "  agent-quit").contains("Essential  On"));
    assert!(line(&mut shell, "  service-kit").contains("Bundled  On"));
    assert!(shows(
        &mut shell,
        "agent-quit, bundled code plugin. 1 permission."
    ));
    assert!(shows(
        &mut shell,
        "service-kit, bundled code plugin. 2 permissions."
    ));
    assert!(shows(&mut shell, "Essential, stays on."));
    assert!(shows(&mut shell, "openai, version 1. Bundled."));
    assert!(shows(&mut shell, "dock-embed, version 1. Added by you."));
    assert!(shows(
        &mut shell,
        "Reaches 127.0.0.1 (local, not encrypted)"
    ));
    assert!(shows(
        &mut shell,
        "the URL's host is not in hosts. (app_plugin_host_undeclared, line 7, http.url)"
    ));
    assert!(shows(&mut shell, "Review this plugin before it runs."));
    assert!(shows(
        &mut shell,
        "Its plugin.toml changed since you approved it. Review it to use it again."
    ));
    assert!(shows(&mut shell, "Set a key for each secret it needs."));
    let listed = rows(&mut shell);
    assert_eq!(
        listed
            .iter()
            .filter(|row| row.contains("OpenAI embeddings") || row.contains("  openai  "))
            .count(),
        1,
        "a plugin the listing carries is not listed again from the grants"
    );
    let at = |title: &str| listed.iter().position(|row| row.contains(title)).unwrap();
    assert!(at("Broken plugin") < at("  agent-quit"));
    assert!(at("  agent-quit") < at("  service-kit"));
}

#[test]
fn a_failed_grants_read_marks_only_the_code_rows_and_r_reads_them_again() {
    let mut fake = Fake::new();
    fake.refuse_grants = Some("owner_busy".into());
    let mut shell = page(&mut fake);
    assert!(line(&mut shell, "Dock embeddings").contains("Approved  On"));
    assert!(shows(&mut shell, "Bundled code plugins couldn't be read."));
    assert!(!shows(&mut shell, "agent-quit"));
    assert!(!shows(&mut shell, "App plugins aren't available"));
    shell.key(Key::Char('r'));
    serve(&mut shell, &mut fake);
    assert!(!shows(&mut shell, "Bundled code plugins couldn't be read."));
    assert!(line(&mut shell, "  agent-quit").contains("Essential  On"));
}

#[test]
fn add_refuses_a_relative_or_long_path_in_the_tui_and_sends_an_absolute_one_as_typed() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    shell.key(Key::Char('a'));
    assert!(shows(&mut shell, "Add an app plugin"));
    assert!(shows(&mut shell, "Plugin folder"));
    shell.key(Key::Paste("plugins/dock-two".into()));
    shell.key(Key::Enter);
    assert!(
        requests(&mut shell).is_empty(),
        "a relative path is not sent"
    );
    assert!(shows(&mut shell, ABSOLUTE));
    shell.key(Key::Ctrl('u'));
    shell.key(Key::Paste(format!("/{}", "a".repeat(PATH_CHARS_MAX))));
    shell.key(Key::Enter);
    assert!(
        requests(&mut shell).is_empty(),
        "a path past 1024 characters is not sent"
    );
    assert!(shows(&mut shell, PATH_LONG));
    shell.key(Key::Ctrl('u'));
    shell.key(Key::Paste("/Users/owner/plugins/missing".into()));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    answer(&mut shell, &mut fake, job);
    assert!(shows(&mut shell, &sentence("app_plugin_not_found")));
    assert!(
        shows(&mut shell, "Add an app plugin"),
        "the form stays open"
    );

    shell.key(Key::Ctrl('u'));
    shell.key(Key::Paste(DOCK_TWO.into()));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(
        matches!(job, AppPluginsJob::Preview { ref folder } if folder.as_path() == std::path::Path::new(DOCK_TWO))
    );
    answer(&mut shell, &mut fake, job);
    assert!(shows(&mut shell, "Review Dock two"));
    assert!(shows(&mut shell, "Embeds on the dock machine."));
    shell.key(Key::Enter);
    assert!(requests(&mut shell).is_empty(), "only y approves a review");
    shell.key(Key::Char('y'));
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::Add { ref folder, ref sha256, replace: false }
            if folder.as_path() == std::path::Path::new(DOCK_TWO) && sha256 == SHA
    ));
    answer(&mut shell, &mut fake, job);
    assert!(!shows(&mut shell, "Review Dock two"));
    assert!(shows(&mut shell, "dock-two, version 1. Added by you."));
    assert!(said(&shell).contains("The plugin is added."));
    assert!(requests(&mut shell).is_empty());
}

#[test]
fn a_changed_plugin_review_shows_what_changed_first_and_y_approves_the_sha_it_showed() {
    let mut fake = Fake::new();
    let mut grown = trust(
        "cove",
        "Cove embeddings",
        http(
            "https://api.cove.invalid/v1/embed",
            &["api.cove.invalid", "api2.cove.invalid"],
            false,
        ),
    );
    grown.changed = true;
    grown.grew_hosts = vec!["api2.cove.invalid".into()];
    fake.review = Preview::Ready { trust: grown };
    let mut shell = page(&mut fake);
    select(&mut shell, 4);
    assert!(bar(&mut shell).contains("Enter Review"));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(matches!(job, AppPluginsJob::Review { ref name } if name == "cove"));
    answer(&mut shell, &mut fake, job);
    let shown = rows(&mut shell);
    let at = |text: &str| {
        shown
            .iter()
            .position(|row| row.contains(text))
            .unwrap_or_else(|| panic!("{text}:\n{}", shown.join("\n")))
    };
    assert!(at("Review Cove embeddings") < at("Changed since you approved it."));
    assert!(at("Changed since you approved it.") < at("Kind: Embedding"));
    assert!(at("New hosts: api2.cove.invalid") < at("Kind: Embedding"));
    assert!(shows(
        &mut shell,
        "Reaches api.cove.invalid, api2.cove.invalid (new)"
    ));
    assert!(shows(&mut shell, &SHA[..32]));
    assert!(bar(&mut shell).contains("y Approve"));
    shell.key(Key::Char('y'));
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::Trust { ref name, ref sha256 } if name == "cove" && sha256 == SHA
    ));
    answer(&mut shell, &mut fake, job);
    assert!(said(&shell).contains("The plugin is approved."));
}

#[test]
fn a_bundled_review_is_read_only_and_space_switches_it() {
    let mut fake = Fake::new();
    fake.review = Preview::Ready {
        trust: trust(
            "ollama",
            "Ollama embeddings",
            http("http://127.0.0.1:11434/api/embed", &["127.0.0.1"], true),
        ),
    };
    let mut shell = page(&mut fake);
    select(&mut shell, 1);
    let hints = bar(&mut shell);
    assert!(hints.contains("Enter Review") && hints.contains("Space Turn off"));
    assert!(!hints.contains("x Remove"), "a bundled plugin offers no x");
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(matches!(job, AppPluginsJob::Review { ref name } if name == "ollama"));
    answer(&mut shell, &mut fake, job);
    assert!(shows(&mut shell, "Ollama embeddings (bundled)"));
    assert!(!bar(&mut shell).contains("Approve"));
    shell.key(Key::Char('y'));
    assert!(requests(&mut shell).is_empty(), "a bundled review has no y");
    shell.key(Key::Esc);
    assert!(!shows(&mut shell, "Ollama embeddings (bundled)"));
    shell.key(Key::Char(' '));
    assert!(matches!(
        one(&mut shell),
        AppPluginsJob::Users { ref name } if name == "ollama"
    ));
}

#[test]
fn turning_off_confirms_with_the_users_and_the_stored_choice_and_tab_changes_this_call_only() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    select(&mut shell, 2);
    assert!(bar(&mut shell).contains("Enter Turn off"));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(matches!(job, AppPluginsJob::Users { ref name } if name == "dock-embed"));
    answer(&mut shell, &mut fake, job);
    assert!(shows(&mut shell, "Turn off Dock embeddings?"));
    assert!(shows(&mut shell, "In use by:"));
    assert!(shows(&mut shell, "- Fix the login page"));
    assert!(shows(
        &mut shell,
        "Refuse new calls: running work goes on without it"
    ));
    assert!(shows(&mut shell, "This is your setting."));
    assert!(bar(&mut shell).contains("Tab Stop runs instead"));
    shell.key(Key::Tab);
    assert!(shows(
        &mut shell,
        "Stop runs using it: the runs listed above stop."
    ));
    assert!(shows(
        &mut shell,
        "This time only. Your setting stays Refuse new calls."
    ));
    shell.key(Key::Char('y'));
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::SetEnabled { ref name, enabled: false, when_off: Some(TurnOff::StopRuns) }
            if name == "dock-embed"
    ));
    answer(&mut shell, &mut fake, job);
    assert!(
        matches!(one(&mut shell), AppPluginsJob::Grants),
        "grants read after a switch"
    );
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(AppPluginsEvent::Grants(Ok(fake.grants.clone()))),
    );
    assert!(line(&mut shell, "Dock embeddings").contains("Approved  Off"));

    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::SetEnabled {
            enabled: true,
            when_off: None,
            ..
        }
    ));
    answer(&mut shell, &mut fake, job);
    serve(&mut shell, &mut fake);
    shell.key(Key::Enter);
    let job = one(&mut shell);
    answer(&mut shell, &mut fake, job);
    assert!(
        shows(&mut shell, "This is your setting."),
        "a second turn-off starts from the setting"
    );
    shell.key(Key::Char('y'));
    assert!(matches!(
        one(&mut shell),
        AppPluginsJob::SetEnabled {
            enabled: false,
            when_off: None,
            ..
        }
    ));
}

#[test]
fn w_switches_the_stored_turn_off_choice_and_names_a_project_override() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    assert!(shows(&mut shell, "When turned off: Refuse new calls"));
    assert!(!shows(&mut shell, "A project settings file sets this"));
    assert!(bar(&mut shell).contains("w When turned off"));
    shell.key(Key::Char('w'));
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::SetWhenOff {
            when_off: TurnOff::StopRuns
        }
    ));
    answer(&mut shell, &mut fake, job);
    assert!(shows(&mut shell, "When turned off: Stop runs using it"));
    assert!(said(&shell).contains("Saved. When turned off: Stop runs using it."));
    select(&mut shell, 2);
    shell.key(Key::Enter);
    let job = one(&mut shell);
    answer(&mut shell, &mut fake, job);
    assert!(
        shows(
            &mut shell,
            "Stop runs using it: the runs listed above stop."
        ),
        "the confirm starts from the saved choice"
    );
    shell.key(Key::Esc);

    fake.refuse = Some("app_plugin_setting_unsafe".into());
    shell.key(Key::Char('w'));
    let job = one(&mut shell);
    answer(&mut shell, &mut fake, job);
    assert!(said(&shell).contains("Not saved. Your settings file is a link"));
    assert!(shows(&mut shell, "When turned off: Stop runs using it"));

    fake.project_override = true;
    shell.key(Key::Char('r'));
    serve(&mut shell, &mut fake);
    assert!(shows(
        &mut shell,
        "A project settings file sets this, and it wins over what you save here."
    ));
}

#[test]
fn the_essential_row_has_no_switch_and_enter_sends_nothing() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    select(&mut shell, 6);
    assert!(
        rows(&mut shell)
            .iter()
            .any(|row| row.starts_with("  > agent-quit"))
    );
    let hints = bar(&mut shell);
    assert!(!hints.contains("Enter"), "{hints}");
    assert!(!hints.contains("Space"), "{hints}");
    shell.key(Key::Enter);
    shell.key(Key::Char(' '));
    assert!(requests(&mut shell).is_empty());
    assert!(said(&shell).is_empty());

    select(&mut shell, 7);
    assert!(bar(&mut shell).contains("Enter Turn off"));
    shell.key(Key::Enter);
    assert!(matches!(
        one(&mut shell),
        AppPluginsJob::Users { ref name } if name == "service-kit"
    ));
}

#[test]
fn remove_confirms_naming_the_entered_key_and_bundled_rows_offer_no_x() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    for bundled in [0, 6] {
        select(&mut shell, bundled);
        shell.key(Key::Char('x'));
        assert!(requests(&mut shell).is_empty());
        assert!(!shows(&mut shell, "Remove "));
        assert!(said(&shell).contains("Bundled plugins can be turned off but not removed"));
    }
    select(&mut shell, 3);
    shell.key(Key::Char('x'));
    assert!(shows(&mut shell, "Remove Harbor embeddings?"));
    assert!(
        !shows(&mut shell, "Removing it also deletes the key"),
        "an account's key is not deleted"
    );
    shell.key(Key::Esc);
    assert_eq!(
        shell.place(),
        Some(Place::AppPlugins),
        "Esc closes the confirm only"
    );
    select(&mut shell, 2);
    assert!(bar(&mut shell).contains("x Remove"));
    shell.key(Key::Char('x'));
    assert!(shows(&mut shell, "Remove Dock embeddings?"));
    assert!(shows(
        &mut shell,
        "Removing it also deletes the key you entered for it."
    ));
    shell.key(Key::Char('y'));
    let job = one(&mut shell);
    assert!(matches!(job, AppPluginsJob::Remove { ref name } if name == "dock-embed"));
    answer(&mut shell, &mut fake, job);
    assert!(!shows(&mut shell, "Dock embeddings"));
    assert!(said(&shell).contains("The plugin is removed."));
}

#[test]
fn set_key_names_the_host_it_goes_to_binds_an_api_key_account_or_sends_a_masked_key() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    select(&mut shell, 0);
    shell.key(Key::Char('k'));
    let sent = requests(&mut shell);
    assert!(matches!(sent.as_slice(), [SettingsJob::AllAccounts]));
    arrive(
        &mut shell,
        SettingsEvent::AllAccounts(Ok(fake.accounts.clone())),
    );
    assert!(shows(&mut shell, "Key for api_key"));
    assert!(shows(&mut shell, "The key goes only to api.openai.com."));
    assert!(shows(&mut shell, "Key from Work OpenAI"));
    assert!(!shows(&mut shell, "Key from Claude Pro"));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::BindAccount { ref name, ref secret, ref account }
            if name == "openai" && secret == "api_key" && account == "acct-002"
    ));
    answer(&mut shell, &mut fake, job);
    assert!(shows(&mut shell, "api_key: Key from Work OpenAI"));

    shell.key(Key::Char('k'));
    let _ = requests(&mut shell);
    arrive(
        &mut shell,
        SettingsEvent::AllAccounts(Ok(fake.accounts.clone())),
    );
    shell.key(Key::Down);
    assert!(shows(
        &mut shell,
        "Sent to the engine once and never shown again."
    ));
    shell.key(Key::Enter);
    assert!(requests(&mut shell).is_empty(), "an empty key is not sent");
    shell.key(Key::Paste(SYNTHETIC_KEY.into()));
    assert!(
        !rows(&mut shell)
            .iter()
            .any(|row| row.contains(SYNTHETIC_KEY))
    );
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(!format!("{job:?}").contains(SYNTHETIC_KEY));
    assert!(matches!(job, AppPluginsJob::SetKey { ref name, .. } if name == "openai"));
    answer(&mut shell, &mut fake, job);
    assert!(shows(&mut shell, "api_key: Key you entered"));
}

#[test]
fn u_unbinds_the_bound_secret() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    select(&mut shell, 3);
    assert!(bar(&mut shell).contains("u Unbind"));
    shell.key(Key::Char('u'));
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::Unbind { ref name, ref secret } if name == "harbor" && secret == "api_key"
    ));
    answer(&mut shell, &mut fake, job);
    assert!(!shows(&mut shell, "api_key: Key from Work OpenAI"));
}

#[test]
fn a_refusal_shows_its_sentence_and_every_app_plugin_code_has_one() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    select(&mut shell, 7);
    shell.key(Key::Enter);
    let job = one(&mut shell);
    answer(&mut shell, &mut fake, job);
    fake.refuse = Some("app_plugin_essential".into());
    shell.key(Key::Char('y'));
    let job = one(&mut shell);
    answer(&mut shell, &mut fake, job);
    assert!(said(&shell).contains("Not saved. The app relies on this plugin, so it stays on."));
    assert!(!shows(&mut shell, "Turn off service-kit?"));

    let mut checked = 0;
    for code in APP_PLUGINS_CODES
        .iter()
        .filter(|code| code.starts_with("app_plugin"))
    {
        assert!(
            !sentence(code).contains(code),
            "{code} has its own sentence"
        );
        checked += 1;
    }
    assert!(checked >= 20, "{checked}");
    assert_eq!(
        sentence("app_plugin_key_store_unavailable"),
        "CrazyAgent couldn't reach its saved keys. Nothing was changed; try again."
    );
}

/// A plugin that is gone says how to see the current list, since neither a failed save
/// nor a failed check reads the list again.
#[test]
fn a_plugin_that_is_gone_says_how_to_see_the_current_ones() {
    assert_eq!(
        sentence("app_plugin_not_found"),
        "That plugin is gone. Open App plugins again to see the current ones."
    );
}

#[test]
fn the_permission_words_shown_are_the_words_the_page_reply_carried() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    select(&mut shell, 7);
    assert!(shows(&mut shell, "- Install a background service"));
    assert!(shows(&mut shell, "- Read a background service's status"));
    assert!(
        !shows(&mut shell, "- Save what the agents were doing"),
        "words show for the selected row only"
    );
    fake.words[1] = words("service.install", "Install a helper service");
    fake.words.pop();
    shell.key(Key::Char('r'));
    serve(&mut shell, &mut fake);
    assert!(shows(&mut shell, "- Install a helper service"));
    assert!(
        shows(&mut shell, "- service.status"),
        "a missing word shows the name"
    );
}

#[test]
fn each_change_notice_shows_as_one_line_kept_across_reads() {
    let mut fake = Fake::new();
    fake.notices = vec![PluginChanged {
        plugin: "cove".into(),
        trusted_sha256: OLD_SHA.into(),
        current_sha256: SHA.into(),
    }];
    let mut shell = page(&mut fake);
    let notice = "Cove embeddings changed since you approved it. Review it to use it again.";
    let count = |shell: &mut Shell| {
        rows(shell)
            .iter()
            .filter(|row| row.contains(notice))
            .count()
    };
    assert_eq!(count(&mut shell), 1);
    shell.key(Key::Char('r'));
    serve(&mut shell, &mut fake);
    assert_eq!(
        count(&mut shell),
        1,
        "a drained notice is kept, never doubled"
    );
}

#[test]
fn app_plugins_open_from_go_to_only_and_the_settings_tabs_are_unchanged() {
    let mut context = ctx(80, 24);
    assert!(
        targets(&context)
            .iter()
            .any(|target| target.label == "Settings / App plugins"
                && target.kind == "Settings"
                && target.place == Place::AppPlugins)
    );
    assert!(
        crate::tui::screens::SettingsSection::ALL
            .iter()
            .all(|section| section.label() != "App plugins")
    );
    let mut screen = open(&Place::AppPlugins, &mut context);
    assert_eq!(screen.crumbs(), ["Settings", "App plugins"]);
    assert_eq!(screen.place(), Some(Place::AppPlugins));
    assert!(screen.wraps_hints());
    assert!(matches!(screen.key(&Key::Esc, &mut context), Flow::Ignored));
}

#[test]
fn plugin_text_reaches_the_screen_without_control_or_line_bytes() {
    let mut fake = Fake::new();
    fake.listing.plugins[2].title = "Dock\u{1b}[31m red".into();
    fake.listing.plugins[2].description = "first line\nsecond line".into();
    let mut shell = page(&mut fake);
    let shown = rows(&mut shell);
    assert!(shown.iter().all(|row| !row.contains('\u{1b}')));
    assert!(
        shown
            .iter()
            .any(|row| row.contains("first line second line"))
    );
}

/// The key bar rows for one page state at one width and pending count, after checking that
/// every hint shows exactly once. Inputs: the page, width and pending count. Output: the
/// row count.
fn bar_count(page: &AppPluginsPage, width: u16, pending: u64) -> usize {
    let context = ctx(width, 24);
    let hints = page
        .modal(&context)
        .map(|(_, hints)| hints)
        .unwrap_or_else(|| page.hints(&context));
    let lines = wrapped_footer(
        usize::from(width),
        &hints,
        pending,
        &context.theme,
        context.glyphs,
        HINT_ROWS_MAX,
    );
    let text: String = lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.to_string()))
        .collect();
    for item in &hints {
        let shown = format!("{} {}", item.key, item.label);
        assert_eq!(
            text.matches(&shown).count(),
            1,
            "{width} {pending} {shown}: {text}"
        );
    }
    assert!(text.trim_end().ends_with("? Help"));
    lines.len()
}

/// A page fed the fake's reads directly, with row `index` selected. Inputs: the fake and
/// the row. Output: the page.
fn fed(fake: &mut Fake, index: usize) -> AppPluginsPage {
    let mut context = ctx(WIDE.0, WIDE.1);
    let mut page = AppPluginsPage::new();
    page.shown(&mut context);
    for _ in 0..8 {
        let Some(Request::Settings(job)) = context.next_request() else {
            break;
        };
        let SettingsJob::AppPlugins(job) = *job else {
            panic!("the page sent another job");
        };
        let event = fake.answer(*job);
        page.apply(&event, &mut context);
    }
    page.selected = index;
    page
}

#[test]
fn the_key_bar_wraps_as_modelled_and_never_drops_a_hint() {
    let mut fake = Fake::new();
    let model: [(&str, usize, [[usize; 3]; 3]); 4] = [
        ("bundled", 1, [[2, 2, 2], [1, 2, 2], [1, 1, 1]]),
        ("added, keys bound", 2, [[2, 2, 2], [2, 2, 2], [1, 1, 1]]),
        ("changed", 4, [[2, 2, 2], [1, 1, 1], [1, 1, 1]]),
        ("essential", 6, [[1, 2, 2], [1, 1, 1], [1, 1, 1]]),
    ];
    for (mix, index, counts) in model {
        let page = fed(&mut fake, index);
        for (width, row) in [80_u16, 120, 240].into_iter().zip(counts) {
            for (pending, expected) in [0_u64, 1, 12].into_iter().zip(row) {
                assert_eq!(
                    bar_count(&page, width, pending),
                    expected,
                    "{mix} at {width} with {pending} pending"
                );
            }
        }
    }
    let mut modal = fed(&mut fake, 2);
    modal.open = Open::TurnOff(Box::new(TurnOffForm {
        name: "dock-embed".into(),
        title: "Dock embeddings".into(),
        users: fake.users.clone(),
        stored: TurnOff::RefuseNewCalls,
        choice: TurnOff::RefuseNewCalls,
    }));
    for width in [80, 120, 240] {
        for pending in [0, 1, 12] {
            assert_eq!(bar_count(&modal, width, pending), 1);
        }
    }
}

#[test]
fn the_page_and_its_modals_match_their_frames() {
    let mut fake = Fake::new();
    let mut shell = Shell::with(ctx(80, 24), Box::new(AppPluginsPage::new()));
    serve(&mut shell, &mut fake);
    check("app-plugins-80", &render(&mut shell, 80, 24), &[], None);

    let mut wide = Shell::with(ctx(120, 30), Box::new(AppPluginsPage::new()));
    serve(&mut wide, &mut fake);
    check("app-plugins-120", &render(&mut wide, 120, 30), &[], None);

    let mut grown = trust(
        "cove",
        "Cove embeddings",
        http(
            "https://api.cove.invalid/v1/embed",
            &["api.cove.invalid", "api2.cove.invalid"],
            false,
        ),
    );
    grown.changed = true;
    grown.grew_hosts = vec!["api2.cove.invalid".into()];
    fake.review = Preview::Ready { trust: grown };
    select(&mut shell, 4);
    shell.key(Key::Enter);
    let job = one(&mut shell);
    answer(&mut shell, &mut fake, job);
    check(
        "app-plugin-review-80",
        &render(&mut shell, 80, 24),
        &[],
        None,
    );
    shell.key(Key::Esc);

    select(&mut shell, 2);
    shell.key(Key::Enter);
    let job = one(&mut shell);
    answer(&mut shell, &mut fake, job);
    check("app-plugin-off-80", &render(&mut shell, 80, 24), &[], None);
}

/// Opens the Add form and sends `path` for its check. Output: the Preview job it sent.
fn check_path(shell: &mut Shell, path: &str) -> AppPluginsJob {
    shell.key(Key::Char('a'));
    shell.key(Key::Paste(path.into()));
    shell.key(Key::Enter);
    one(shell)
}

#[test]
fn an_add_path_edited_while_its_check_runs_is_checked_again_and_never_added_as_typed_later() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    let job = check_path(&mut shell, DOCK_TWO);
    shell.key(Key::Paste("-copy".into()));
    answer(&mut shell, &mut fake, job);
    assert!(
        !shows(&mut shell, "Review Dock two"),
        "a review of another path is never offered for approval"
    );
    assert!(shows(&mut shell, PATH_CHANGED));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::Preview { ref folder }
            if folder.as_path() == std::path::Path::new("/Users/owner/plugins/dock-two-copy")
    ));
}

#[test]
fn the_review_scroll_stops_at_its_last_line_so_one_up_moves_back_one_line() {
    let mut fake = Fake::new();
    let Preview::Ready { trust } = fake.folders[0].1.clone() else {
        panic!("the dock-two folder has a review");
    };
    let texts = trust_lines(&trust, usize::from(MODAL_WIDTH) - 4);
    let before_last = texts[texts.len() - 2].clone();
    let mut shell = page(&mut fake);
    let job = check_path(&mut shell, DOCK_TWO);
    answer(&mut shell, &mut fake, job);
    assert!(shows(&mut shell, "Review Dock two"));
    for _ in 0..40 {
        shell.key(Key::Down);
    }
    assert!(
        !shows(&mut shell, &before_last),
        "only the last line is left"
    );
    shell.key(Key::Up);
    assert!(
        shows(&mut shell, &before_last),
        "one Up shows the line before the last"
    );
}

#[test]
fn a_turn_off_sent_with_y_shows_it_is_saving_and_offers_no_keep_on() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    select(&mut shell, 2);
    shell.key(Key::Enter);
    let job = one(&mut shell);
    answer(&mut shell, &mut fake, job);
    assert!(bar(&mut shell).contains("Esc Keep on"));
    shell.key(Key::Char('y'));
    let job = one(&mut shell);
    assert!(!bar(&mut shell).contains("Keep on"), "{}", bar(&mut shell));
    assert!(shows(&mut shell, TURNING_OFF));
    shell.key(Key::Esc);
    assert!(requests(&mut shell).is_empty(), "Esc sends no cancel");
    answer(&mut shell, &mut fake, job);
    serve(&mut shell, &mut fake);
    assert!(said(&shell).starts_with("Saved."), "{}", said(&shell));
    assert!(line(&mut shell, "Dock embeddings").contains("Approved  Off"));
}

#[test]
fn the_key_source_list_scrolls_so_the_typed_key_source_and_its_input_stay_in_view() {
    let mut fake = Fake::new();
    fake.accounts = (1..=30)
        .map(|index| account(index, &format!("Key account {index}"), SignInMethod::ApiKey))
        .collect();
    let mut shell = page(&mut fake);
    select(&mut shell, 0);
    shell.key(Key::Char('k'));
    let _ = requests(&mut shell);
    arrive(
        &mut shell,
        SettingsEvent::AllAccounts(Ok(fake.accounts.clone())),
    );
    for _ in 0..30 {
        shell.key(Key::Down);
    }
    let shown = render(&mut shell, 100, 24);
    assert!(
        shown.iter().any(|row| row.contains("Key for api_key")),
        "{shown:#?}"
    );
    assert!(
        shown
            .iter()
            .any(|row| row.contains("Enter a key for this plugin")),
        "{shown:#?}"
    );
    assert!(
        shown
            .iter()
            .any(|row| row.contains("Sent to the engine once and never shown again.")),
        "{shown:#?}"
    );
}

#[test]
fn the_add_path_hint_names_a_path_starting_with_slash() {
    assert_eq!(
        ABSOLUTE,
        "Type the full path of the plugin folder, starting with /."
    );
}

#[test]
fn a_change_waiting_its_turn_reads_waiting_and_other_refusals_still_read_not_saved() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    select(&mut shell, 2);
    shell.key(Key::Char('x'));
    shell.key(Key::Char('y'));
    fake.refuse = Some("access_change_pending".into());
    let job = one(&mut shell);
    answer(&mut shell, &mut fake, job);
    let message = shell.ctx.message().expect("a message").clone();
    assert_eq!(message.lead, "Waiting.");
    assert_eq!(message.rest, crate::config::ACCESS_CHANGE_PENDING_LINE);
    assert_eq!(message.tone, crate::tui::frame::Tone::Warning);

    shell.key(Key::Char('x'));
    shell.key(Key::Char('y'));
    fake.refuse = Some("app_plugin_not_found".into());
    let job = one(&mut shell);
    answer(&mut shell, &mut fake, job);
    assert!(said(&shell).starts_with("Not saved. "), "{}", said(&shell));
}

/// A plugin change whose engine actor stopped before it answered reads as the page's
/// unknown outcome, since the change may have landed.
#[test]
fn a_stopped_owner_actor_reads_as_an_unknown_plugin_outcome() {
    assert_eq!(
        sentence("owner_actor_stopped_outcome_unknown"),
        sentence("app_plugin_outcome_unknown")
    );
}

/// A plugin change whose owner answer was lost reads as the page's unknown outcome, since
/// the change may have landed.
#[test]
fn a_lost_owner_answer_reads_as_an_unknown_plugin_outcome() {
    assert_eq!(
        sentence("owner_outcome_unknown_reconcile_same_identity"),
        sentence("app_plugin_outcome_unknown")
    );
}
