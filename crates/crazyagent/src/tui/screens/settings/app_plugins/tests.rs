use super::*;
use crate::accounts::{AccountStatus, Provider};
use crate::protocol::CredentialRef;
use crate::tui::client::OwnerEvent;
use crate::tui::client::app_plugins::job_name;
use crate::tui::screens::open;
use crate::tui::screens::settings_accounts::tests::{ctx, render};
use crate::tui::seam::Request;
use crate::tui::shell::{Shell, targets};

const SHA: &str = "5f2c0d8e9a7b6c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e4f3a2b1c0d";
const DOCK_TWO: &str = "schema: 1\nname: dock-two\ntitle: Dock two\nkind: embedding\n";
const SYNTHETIC_KEY: &str = "sk-test-tui-secret-77";

/// A stand-in engine for the App plugins page: the listing, the manifests it accepts by
/// text, the trust screen a review shows, the accounts it lists and every job it was sent.
pub(in crate::tui) struct Fake {
    pub listing: Listing,
    pub manifests: Vec<(String, Preview)>,
    pub review: Option<Preview>,
    pub accounts: Vec<AccountView>,
    pub refuse: Option<String>,
    pub seen: Vec<String>,
}

fn problem(code: &str, line: Option<u32>, field: Option<&str>, message: &str) -> Problem {
    Problem {
        code: code.into(),
        line,
        field: field.map(Into::into),
        message: message.into(),
    }
}

fn http(url: &str, host: &str, local: bool) -> RunsView {
    RunsView::Http {
        url: url.into(),
        hosts: vec![host.into()],
        local_unencrypted: local,
        dimensions: 256,
    }
}

fn plugin(
    name: &str,
    title: &str,
    origin: Origin,
    status: PluginStatus,
    enabled: bool,
) -> PluginView {
    PluginView {
        name: name.into(),
        title: title.into(),
        version: "1".into(),
        description: String::new(),
        kind: PluginKind::Embedding,
        origin,
        status,
        enabled,
        runs: None,
        secrets: Vec::new(),
        sha256: SHA.into(),
    }
}

/// The five plugins every row starts from: the built-in model, OpenAI needing its key, the
/// owner's loopback plugin, an unreviewed one bound to an account, and a refused one.
pub(in crate::tui) fn listing() -> Listing {
    let mut local = plugin(
        "local",
        "Built-in model",
        Origin::Bundled,
        PluginStatus::Ready,
        true,
    );
    local.runs = Some(RunsView::Local {
        runtime: "static-retrieval-mrl-en-v1".into(),
        download_bytes: 125_729_604,
    });
    let mut openai = plugin(
        "openai",
        "OpenAI embeddings",
        Origin::Bundled,
        PluginStatus::NeedsKey,
        false,
    );
    openai.runs = Some(http(
        "https://api.openai.com/v1/embeddings",
        "api.openai.com",
        false,
    ));
    openai.secrets = vec![SecretView {
        name: "api_key".into(),
        binding: Binding::None,
    }];
    let mut dock = plugin(
        "dock-embed",
        "Dock embeddings",
        Origin::User,
        PluginStatus::Ready,
        true,
    );
    dock.runs = Some(http("http://127.0.0.1:8080/embed", "127.0.0.1", true));
    let mut harbor = plugin(
        "harbor",
        "Harbor embeddings",
        Origin::User,
        PluginStatus::NeedsReview,
        false,
    );
    harbor.runs = Some(http(
        "https://api.harbor.invalid/v1/embed",
        "api.harbor.invalid",
        false,
    ));
    harbor.secrets = vec![SecretView {
        name: "api_key".into(),
        binding: Binding::Account {
            account: "acct-002".into(),
            name: "Work OpenAI".into(),
        },
    }];
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
        plugins: vec![local, openai, dock, harbor, broken],
        more: false,
    }
}

fn trust(name: &str, title: &str, replaces: bool) -> TrustView {
    TrustView {
        name: name.into(),
        title: title.into(),
        version: "2".into(),
        description: "Embeds on the dock machine.".into(),
        kind: PluginKind::Embedding,
        runs: http("http://127.0.0.1:9090/embed", "127.0.0.1", true),
        secrets: Vec::new(),
        sha256: SHA.into(),
        replaces,
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
    pub(in crate::tui) fn new() -> Self {
        Self {
            listing: listing(),
            manifests: vec![(
                DOCK_TWO.into(),
                Preview::Ready {
                    trust: trust("dock-two", "Dock two", false),
                },
            )],
            review: Some(Preview::Ready {
                trust: trust("harbor", "Harbor embeddings", false),
            }),
            accounts: vec![
                account(1, "Claude Pro", SignInMethod::Subscription),
                account(2, "Work OpenAI", SignInMethod::ApiKey),
            ],
            refuse: None,
            seen: Vec::new(),
        }
    }

    fn find(&mut self, name: &str) -> Option<&mut PluginView> {
        self.listing
            .plugins
            .iter_mut()
            .find(|plugin| plugin.name == name)
    }

    fn changed(&mut self) -> AppPluginsEvent {
        match self.refuse.take() {
            Some(code) => AppPluginsEvent::Listing(Err(code)),
            None => AppPluginsEvent::Listing(Ok(self.listing.clone())),
        }
    }

    /// Answers one job the way the engine would, refusing a trust whose SHA-256 is not the
    /// one the trust screen showed. Input: the job. Output: the event.
    pub(in crate::tui) fn answer(&mut self, job: AppPluginsJob) -> AppPluginsEvent {
        self.seen.push(job_name(&job).into());
        match job {
            AppPluginsJob::List => AppPluginsEvent::Listing(Ok(self.listing.clone())),
            AppPluginsJob::Preview { yaml } => AppPluginsEvent::Preview {
                read: None,
                result: self
                    .manifests
                    .iter()
                    .find(|(text, _)| *text == yaml)
                    .map(|(_, preview)| preview.clone())
                    .ok_or_else(|| "app_plugin_request_invalid".into()),
            },
            AppPluginsJob::ReadFile { path } => match path.to_str() {
                Some("/Users/owner/plugins/dock-two.yaml") => AppPluginsEvent::Preview {
                    read: Some(DOCK_TWO.into()),
                    result: Ok(self.manifests[0].1.clone()),
                },
                _ => AppPluginsEvent::Preview {
                    read: None,
                    result: Err("app_plugin_file_unreadable".into()),
                },
            },
            AppPluginsJob::Review { .. } => AppPluginsEvent::Preview {
                read: None,
                result: self
                    .review
                    .clone()
                    .ok_or_else(|| "app_plugin_not_found".into()),
            },
            AppPluginsJob::Add {
                yaml,
                sha256,
                replace,
            } => {
                assert_eq!(
                    sha256, SHA,
                    "the add carries the SHA-256 the trust screen showed"
                );
                assert!(self.manifests.iter().any(|(text, _)| *text == yaml));
                if !replace {
                    let mut added = plugin(
                        "dock-two",
                        "Dock two",
                        Origin::User,
                        PluginStatus::Ready,
                        false,
                    );
                    added.runs = Some(http("http://127.0.0.1:9090/embed", "127.0.0.1", true));
                    self.listing.plugins.push(added);
                }
                self.changed()
            }
            AppPluginsJob::Trust { name, sha256 } => {
                assert_eq!(sha256, SHA);
                if let Some(plugin) = self.find(&name) {
                    plugin.status = PluginStatus::Ready;
                }
                self.changed()
            }
            AppPluginsJob::SetEnabled { name, enabled } => {
                if let Some(plugin) = self.find(&name) {
                    plugin.enabled = enabled;
                }
                self.changed()
            }
            AppPluginsJob::Remove { name } => {
                self.listing.plugins.retain(|plugin| plugin.name != name);
                self.changed()
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
                self.changed()
            }
            AppPluginsJob::SetKey { name, secret, .. } => {
                if self.refuse.is_none()
                    && let Some(plugin) = self.find(&name)
                    && let Some(one) = plugin.secrets.iter_mut().find(|one| one.name == secret)
                {
                    one.binding = Binding::Key;
                }
                self.changed()
            }
            AppPluginsJob::Unbind { name, secret } => {
                if let Some(plugin) = self.find(&name)
                    && let Some(one) = plugin.secrets.iter_mut().find(|one| one.name == secret)
                {
                    one.binding = Binding::None;
                }
                self.changed()
            }
            other => AppPluginsEvent::unavailable(&other),
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

/// Answers every request the page sent, and the ones those answers lead to.
fn serve(shell: &mut Shell, fake: &mut Fake) {
    for _ in 0..8 {
        let sent = requests(shell);
        if sent.is_empty() {
            return;
        }
        for job in sent {
            let event = match job {
                SettingsJob::AppPlugins(job) => {
                    SettingsEvent::AppPlugins(Box::new(fake.answer(*job)))
                }
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

fn shows(shell: &mut Shell, text: &str) -> bool {
    render(shell, 100, 40).iter().any(|row| row.contains(text))
}

fn page(fake: &mut Fake) -> Shell {
    let mut shell = Shell::with(ctx(100, 40), Box::new(AppPluginsPage::new()));
    serve(&mut shell, fake);
    shell
}

fn select(shell: &mut Shell, index: usize) {
    for _ in 0..8 {
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

#[test]
fn a1_every_plugin_shows_one_status_label_where_it_came_from_and_how_it_runs() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    let rows = render(&mut shell, 100, 40);
    let line = |title: &str| {
        rows.iter()
            .find(|row| row.contains(title))
            .cloned()
            .unwrap_or_default()
    };
    assert!(line("Built-in model").contains("Enabled"));
    assert!(line("OpenAI embeddings").contains("Needs a key"));
    assert!(line("Dock embeddings").contains("Enabled"));
    assert!(line("Harbor embeddings").contains("Needs review"));
    assert!(line("Broken plugin").contains("Refused"));
    assert!(shows(&mut shell, "local, version 1. Built in."));
    assert!(shows(&mut shell, "dock-embed, version 1. Added by you."));
    assert!(shows(
        &mut shell,
        "Runs on this computer with the built-in model, a 126 MB download"
    ));
    assert!(shows(
        &mut shell,
        "Reaches 127.0.0.1 (local, not encrypted)"
    ));
    assert!(shows(&mut shell, "Reaches api.openai.com"));
    assert!(shows(
        &mut shell,
        "the URL's host is not in hosts. (app_plugin_host_undeclared, line 7, http.url)"
    ));
    assert!(shows(&mut shell, "Review this plugin before it runs."));
    assert!(shows(&mut shell, "Set a key for each secret it needs."));
    assert_eq!(fake.seen, ["AppPlugins"]);
}

#[test]
fn a6_a7_enter_turns_a_usable_plugin_off_and_on_and_a_refused_one_says_why() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    shell.key(Key::Enter);
    assert!(matches!(
        one(&mut shell),
        AppPluginsJob::SetEnabled { ref name, enabled: false } if name == "local"
    ));
    fake.listing.plugins[0].enabled = false;
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(AppPluginsEvent::Listing(Ok(fake.listing.clone())))),
    );
    let rows = render(&mut shell, 100, 40);
    assert!(
        rows.iter()
            .any(|row| row.contains("Built-in model") && row.contains("Off"))
    );
    shell.key(Key::Char(' '));
    assert!(matches!(
        one(&mut shell),
        AppPluginsJob::SetEnabled { ref name, enabled: true } if name == "local"
    ));
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(AppPluginsJob::SetEnabled {
            name: "local".into(),
            enabled: true,
        }))),
    );

    select(&mut shell, 4);
    shell.key(Key::Enter);
    assert!(
        requests(&mut shell).is_empty(),
        "a refused plugin cannot be turned on"
    );
    assert!(said(&shell).contains("Refused plugins can't run."));
}

#[test]
fn a9_enter_reviews_an_unreviewed_plugin_and_y_trusts_the_sha_it_showed() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    select(&mut shell, 3);
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(matches!(job, AppPluginsJob::Review { ref name } if name == "harbor"));
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );
    assert!(shows(&mut shell, "Review Harbor embeddings"));
    assert!(shows(&mut shell, "Kind: Embedding"));
    assert!(shows(
        &mut shell,
        "Reaches 127.0.0.1 (local, not encrypted)"
    ));
    assert!(shows(&mut shell, "Address: http://127.0.0.1:9090/embed"));
    assert!(shows(&mut shell, "Secrets it needs: none"));
    assert!(shows(&mut shell, "SHA-256"));
    assert!(shows(&mut shell, &SHA[..32]));
    shell.key(Key::Char('y'));
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::Trust { ref name, ref sha256 } if name == "harbor" && sha256 == SHA
    ));
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );
    assert!(!shows(&mut shell, "Review Harbor embeddings"));
    assert!(!shows(&mut shell, "Needs review"));
}

#[test]
fn a8_only_the_owners_plugins_are_removed_and_only_after_y() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    shell.key(Key::Char('x'));
    assert!(requests(&mut shell).is_empty());
    assert!(said(&shell).contains("Built-in plugins can be turned off but not removed"));
    select(&mut shell, 2);
    shell.key(Key::Char('x'));
    assert!(shows(&mut shell, "Remove Dock embeddings?"));
    assert!(shows(
        &mut shell,
        "Its manifest is deleted from your plugins folder."
    ));
    shell.key(Key::Esc);
    assert!(requests(&mut shell).is_empty());
    assert_eq!(
        shell.place(),
        Some(Place::AppPlugins),
        "Esc closes the confirm, not the page"
    );
    shell.key(Key::Char('x'));
    shell.key(Key::Char('y'));
    let job = one(&mut shell);
    assert!(matches!(job, AppPluginsJob::Remove { ref name } if name == "dock-embed"));
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );
    assert!(!shows(&mut shell, "Dock embeddings"));
}

#[test]
fn a2_a3_a13_a_pasted_manifest_is_checked_and_added_only_after_y() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    shell.key(Key::Char('a'));
    assert!(shows(&mut shell, "Add an app plugin"));
    assert!(shows(&mut shell, "Plugin manifest (YAML)"));
    assert!(shows(&mut shell, "Or the path of a .yaml file"));
    shell.key(Key::Enter);
    assert!(requests(&mut shell).is_empty(), "Check waits for text");
    shell.key(Key::Paste(DOCK_TWO.replace('\n', "\r\n")));
    shell.key(Key::Char('?'));
    shell.key(Key::Backspace);
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(matches!(job, AppPluginsJob::Preview { ref yaml } if yaml == DOCK_TWO));
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );
    assert!(shows(&mut shell, "Trust Dock two?"));
    assert!(shows(&mut shell, "Embeds on the dock machine."));
    shell.key(Key::Enter);
    assert!(
        requests(&mut shell).is_empty(),
        "only y confirms a trust screen"
    );
    shell.key(Key::Char('y'));
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::Add { ref yaml, ref sha256, replace: false } if yaml == DOCK_TWO && sha256 == SHA
    ));
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );
    assert!(!shows(&mut shell, "Trust Dock two?"));
    assert!(shows(&mut shell, "dock-two, version 1. Added by you."));
    assert!(said(&shell).contains("The plugin is added."));
}

#[test]
fn a14_a_manifest_naming_the_owners_plugin_says_it_replaces_it() {
    let mut fake = Fake::new();
    fake.manifests[0].1 = Preview::Ready {
        trust: trust("dock-embed", "Dock embeddings", true),
    };
    let mut shell = page(&mut fake);
    shell.key(Key::Char('a'));
    shell.key(Key::Paste(DOCK_TWO.into()));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );
    assert!(shows(&mut shell, "Replaces your plugin named dock-embed."));
    shell.key(Key::Char('y'));
    assert!(matches!(
        one(&mut shell),
        AppPluginsJob::Add { replace: true, .. }
    ));
}

#[test]
fn a5_a_refused_manifest_shows_its_problem_offers_no_confirm_and_e_edits_it() {
    let mut fake = Fake::new();
    fake.manifests[0].1 = Preview::Refused {
        problem: problem(
            "app_plugin_hosts_limit",
            Some(3),
            Some("hosts"),
            "a plugin declares at most 4 hosts.",
        ),
    };
    let mut shell = page(&mut fake);
    shell.key(Key::Char('a'));
    shell.key(Key::Paste(DOCK_TWO.into()));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );
    assert!(shows(&mut shell, "This manifest can't be added"));
    assert!(shows(
        &mut shell,
        "a plugin declares at most 4 hosts. (app_plugin_hosts_limit,"
    ));
    assert!(shows(&mut shell, "line 3, hosts)"));
    shell.key(Key::Char('y'));
    assert!(
        requests(&mut shell).is_empty(),
        "a refused manifest has no confirm"
    );
    shell.key(Key::Char('e'));
    assert!(shows(&mut shell, "Add an app plugin"));
    assert!(
        shows(&mut shell, "kind: embedding"),
        "the text comes back to edit"
    );
}

#[test]
fn a4_a_typed_absolute_path_is_read_by_the_owner_worker_and_its_text_is_what_is_added() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    shell.key(Key::Char('a'));
    shell.key(Key::Tab);
    shell.key(Key::Paste("dock-two.yaml".into()));
    shell.key(Key::Enter);
    assert!(requests(&mut shell).is_empty());
    assert!(shows(
        &mut shell,
        "Type the full path of a .yaml file, starting with /."
    ));
    for _ in 0.."dock-two.yaml".len() {
        shell.key(Key::Backspace);
    }
    shell.key(Key::Paste("/Users/owner/plugins/dock-two.yaml".into()));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::ReadFile { ref path } if path == std::path::Path::new("/Users/owner/plugins/dock-two.yaml")
    ));
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );
    assert!(shows(&mut shell, "Trust Dock two?"));
    shell.key(Key::Char('y'));
    let job = one(&mut shell);
    assert!(matches!(job, AppPluginsJob::Add { ref yaml, .. } if yaml == DOCK_TWO));
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );

    shell.key(Key::Char('a'));
    shell.key(Key::Tab);
    shell.key(Key::Paste("/Users/owner/plugins/missing.yaml".into()));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );
    assert!(shows(&mut shell, "CrazyAgent couldn't read that file."));
    assert!(
        shows(&mut shell, "Add an app plugin"),
        "the form stays open to fix the path"
    );
}

#[test]
fn a_manifest_past_16_kib_is_refused_whole_never_cut() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    shell.key(Key::Char('a'));
    shell.key(Key::Paste("#".repeat(MANIFEST_BYTES_MAX + 40)));
    shell.key(Key::Enter);
    assert!(requests(&mut shell).is_empty(), "nothing cut is sent");
    assert!(shows(&mut shell, "A manifest can be at most 16 KB."));
}

#[test]
fn a10_a11_k_binds_a_secret_to_an_api_key_account_only() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    select(&mut shell, 1);
    shell.key(Key::Char('k'));
    let sent = requests(&mut shell);
    assert!(matches!(sent.as_slice(), [SettingsJob::AllAccounts]));
    arrive(
        &mut shell,
        SettingsEvent::AllAccounts(Ok(fake.accounts.clone())),
    );
    assert!(shows(&mut shell, "Key for api_key"));
    assert!(shows(&mut shell, "Key from Work OpenAI"));
    assert!(
        !shows(&mut shell, "Key from Claude Pro"),
        "a subscription account has no key to share"
    );
    assert!(shows(&mut shell, "Enter a key for this plugin"));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::BindAccount { ref name, ref secret, ref account }
            if name == "openai" && secret == "api_key" && account == "acct-002"
    ));
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );
    assert!(shows(&mut shell, "api_key: Key from Work OpenAI"));
}

#[test]
fn a10_a12_a_typed_key_is_masked_sent_once_and_never_drawn() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    select(&mut shell, 1);
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
    assert!(shows(&mut shell, "That key can't be used"));
    shell.key(Key::Paste(SYNTHETIC_KEY.into()));
    assert!(
        !render(&mut shell, 100, 40)
            .iter()
            .any(|row| row.contains(SYNTHETIC_KEY))
    );
    fake.refuse = Some("app_plugin_key_store_unavailable".into());
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(
        matches!(job, AppPluginsJob::SetKey { ref name, ref secret, .. } if name == "openai" && secret == "api_key")
    );
    assert!(!format!("{job:?}").contains(SYNTHETIC_KEY));
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );
    assert!(shows(
        &mut shell,
        "CrazyAgent couldn't save the key. Nothing was saved."
    ));
    assert!(
        !render(&mut shell, 100, 40)
            .iter()
            .any(|row| row.contains(SYNTHETIC_KEY))
    );
    shell.key(Key::Enter);
    assert!(
        requests(&mut shell).is_empty(),
        "the refused key was taken, not kept to send again"
    );
    shell.key(Key::Paste(SYNTHETIC_KEY.into()));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );
    assert!(shows(&mut shell, "api_key: Key you entered"));
}

#[test]
fn a11_u_unbinds_the_bound_secret() {
    let mut fake = Fake::new();
    let mut shell = page(&mut fake);
    assert!(shows(&mut shell, "api_key: Key from Work OpenAI"));
    select(&mut shell, 3);
    shell.key(Key::Char('u'));
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::Unbind { ref name, ref secret } if name == "harbor" && secret == "api_key"
    ));
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(fake.answer(job))),
    );
    assert!(!shows(&mut shell, "api_key: Key from Work OpenAI"));
}

#[test]
fn a15_a_listing_with_more_plugins_than_it_carries_says_so() {
    let mut fake = Fake::new();
    fake.listing.more = true;
    let mut shell = page(&mut fake);
    shell.key(Key::Down);
    for _ in 0..5 {
        shell.key(Key::Down);
    }
    assert!(shows(&mut shell, "Showing the first 5 plugins."));
}

#[test]
fn manifest_text_reaches_the_screen_without_control_or_line_bytes() {
    let mut fake = Fake::new();
    fake.listing.plugins[2].title = "Dock\u{1b}[31m red".into();
    fake.listing.plugins[2].description = "first line\nsecond line".into();
    let mut shell = page(&mut fake);
    let rows = render(&mut shell, 100, 40);
    assert!(rows.iter().all(|row| !row.contains('\u{1b}')));
    assert!(
        rows.iter()
            .any(|row| row.contains("first line second line"))
    );
}

#[test]
fn app_plugins_open_from_go_to_and_a_refused_read_offers_r() {
    let mut context = ctx(80, 24);
    assert!(
        targets(&context)
            .iter()
            .any(|target| target.label == "Settings / App plugins"
                && target.kind == "Settings"
                && target.place == Place::AppPlugins)
    );
    let mut screen = open(&Place::AppPlugins, &mut context);
    assert_eq!(screen.crumbs(), ["Settings", "App plugins"]);
    assert_eq!(screen.place(), Some(Place::AppPlugins));
    assert!(matches!(screen.key(&Key::Esc, &mut context), Flow::Ignored));

    let mut shell = Shell::with(ctx(100, 40), Box::new(AppPluginsPage::new()));
    let job = one(&mut shell);
    assert!(matches!(job, AppPluginsJob::List));
    arrive(
        &mut shell,
        SettingsEvent::AppPlugins(Box::new(AppPluginsEvent::Listing(Err(
            "app_plugins_unavailable".into(),
        )))),
    );
    assert!(shows(
        &mut shell,
        "App plugins aren't available in this engine yet."
    ));
    shell.key(Key::Char('r'));
    assert!(matches!(one(&mut shell), AppPluginsJob::List));
}
