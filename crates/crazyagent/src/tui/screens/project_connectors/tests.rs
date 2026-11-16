use super::*;
use crate::app_plugins::{Binding, Origin, PluginStatus, Problem, RunsView, SecretView};
use crate::connectors::approvals::AllowedWrites;
use crate::connectors::{
    ConnectorSettings, ConnectorView, ConnectorsView, Grant, SignIn, Who, WritePolicy,
};
use crate::tui::client::OwnerEvent;
use crate::tui::client::connectors::{ConnectorsEvent, ConnectorsJob};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::render_tests::{check, ready, render, shell_at};
use crate::tui::seam::{Arrival, Request};
use crate::tui::shell::Shell;

const PROJECT: &str = "alpha";

fn connector(name: &str, title: &str, status: PluginStatus) -> ConnectorView {
    ConnectorView {
        name: name.into(),
        title: title.into(),
        version: "1.0.0".into(),
        description: String::new(),
        origin: Origin::Bundled,
        status,
        sign_in: SignIn::Key,
        runs: Some(RunsView::Connector {
            url: format!("https://{name}.example.com/mcp"),
            hosts: vec![format!("{name}.example.com")],
            oauth_hosts: Vec::new(),
            vendor_read_only: false,
        }),
        secrets: vec![SecretView {
            name: "api_key".into(),
            binding: Binding::Key,
        }],
        sha256: "c0ffee".into(),
    }
}

fn refused_file(name: &str) -> ConnectorView {
    let problem = Problem {
        code: "app_plugin_name_taken".into(),
        line: None,
        field: Some("name".into()),
        message: "a plugin with this name already exists; remove it or choose replace.".into(),
    };
    let mut file = connector(name, name, PluginStatus::Refused { problem });
    file.version = String::new();
    file.origin = Origin::User;
    file.sign_in = SignIn::NotYet;
    file.runs = None;
    file.secrets.clear();
    file
}

fn grant(connector: &str, enabled: bool, who: Who) -> Grant {
    Grant {
        connector: connector.into(),
        enabled,
        who,
    }
}

fn view(grants: Vec<Grant>) -> ConnectorsView {
    ConnectorsView {
        connectors: vec![
            connector("linear", "Linear", PluginStatus::Ready),
            connector("github", "GitHub", PluginStatus::Ready),
            connector("board", "Board", PluginStatus::NeedsReview),
        ],
        more: false,
        settings: ConnectorSettings::DEFAULT,
        version: 3,
        project: Some(PROJECT.into()),
        grants,
    }
}

fn seeded() -> ConnectorsView {
    view(vec![
        grant("linear", true, Who::Master),
        grant("gone", true, Who::Both),
    ])
}

fn requests(shell: &mut Shell) -> Vec<ConnectorsJob> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Settings(job) => match *job {
                SettingsJob::Connectors(job) => Some(*job),
                _ => panic!("the page sent another Settings request"),
            },
            _ => None,
        })
        .collect()
}

fn one(shell: &mut Shell) -> ConnectorsJob {
    let mut sent = requests(shell);
    assert_eq!(sent.len(), 1, "one request at a time");
    sent.remove(0)
}

fn arrive(shell: &mut Shell, event: ConnectorsEvent) {
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Connectors(Box::new(event)),
    )))));
}

fn reads_project(job: &ConnectorsJob) -> bool {
    matches!(job, ConnectorsJob::View { project: Some(project) } if project == PROJECT)
}

fn page(read: ConnectorsView) -> Shell {
    let place = Place::ProjectConnectors {
        id: ProjectId::new(PROJECT).unwrap(),
        name: "Plant shop".into(),
    };
    let mut shell = shell_at(ready(80, 24), place);
    assert!(reads_project(&one(&mut shell)), "the page reads its project");
    arrive(&mut shell, ConnectorsEvent::Page(Ok(read)));
    shell
}

fn shows(shell: &mut Shell, text: &str) -> bool {
    render(shell, 80, 24).iter().any(|row| row.contains(text))
}

fn said(shell: &Shell) -> String {
    shell
        .ctx
        .message()
        .map(|message| format!("{} {}", message.lead, message.rest))
        .unwrap_or_default()
}

fn keys(shell: &mut Shell, keys: &[Key]) {
    for key in keys {
        shell.key(key.clone());
    }
}

fn granted(job: &ConnectorsJob, connector: &str, enabled: bool, who: Who) -> bool {
    matches!(
        job,
        ConnectorsJob::Grant { project, connector: name, enabled: on, who: reach }
            if project == PROJECT && name == connector && *on == enabled && *reach == who
    )
}

#[test]
fn the_section_lists_ready_and_on_connectors_with_who_and_names_the_rest_once() {
    let mut shell = page(seeded());
    check("project-connectors-80", &render(&mut shell, 80, 24), &[]);
}

#[test]
fn space_on_an_off_connector_turns_it_on_for_master_and_agents() {
    let mut shell = page(seeded());
    keys(&mut shell, &[Key::Down, Key::Down]);
    assert!(shows(&mut shell, "Space Turn on"));
    keys(&mut shell, &[Key::Char(' ')]);
    let job = one(&mut shell);
    assert!(granted(&job, "github", true, Who::Both), "{job:?}");
    let mut on = seeded();
    on.grants.push(grant("github", true, Who::Both));
    arrive(&mut shell, ConnectorsEvent::Page(Ok(on)));
    assert!(shows(&mut shell, "Master and agents with tools on"));
    let mut shell = page(view(Vec::new()));
    keys(&mut shell, &[Key::Enter]);
    assert!(granted(&one(&mut shell), "linear", true, Who::Both));
}

#[test]
fn turning_one_off_keeps_its_who_and_a_gone_one_turns_off_too() {
    let mut shell = page(seeded());
    assert!(shows(&mut shell, "Space Turn off"));
    keys(&mut shell, &[Key::Char(' ')]);
    assert!(granted(&one(&mut shell), "linear", false, Who::Master));
    arrive(
        &mut shell,
        ConnectorsEvent::Page(Ok(view(vec![
            grant("linear", false, Who::Master),
            grant("gone", true, Who::Both),
        ]))),
    );
    assert!(!shows(&mut shell, "Who can use it"), "an Off connector has no who");
    keys(&mut shell, &[Key::End, Key::Char(' ')]);
    assert!(granted(&one(&mut shell), "gone", false, Who::Both));
}

#[test]
fn left_and_right_on_who_send_each_choice_in_the_desktop_s_order() {
    let mut shell = page(seeded());
    keys(&mut shell, &[Key::Down]);
    assert!(shows(&mut shell, "> ") && shows(&mut shell, "Who can use it  Master only"));
    for (key, who, words) in [
        (Key::Right, Who::Agents, "Who can use it  Agents only"),
        (Key::Right, Who::Both, "Who can use it  Master and agents with tools on"),
        (Key::Right, Who::Master, "Who can use it  Master only"),
        (Key::Left, Who::Both, "Who can use it  Master and agents with tools on"),
    ] {
        keys(&mut shell, &[key]);
        let job = one(&mut shell);
        assert!(granted(&job, "linear", true, who), "{who:?}: {job:?}");
        arrive(
            &mut shell,
            ConnectorsEvent::Page(Ok(view(vec![
                grant("linear", true, who),
                grant("gone", true, Who::Both),
            ]))),
        );
        assert!(shows(&mut shell, words), "{words}");
    }
    keys(&mut shell, &[Key::Char(' ')]);
    assert!(
        requests(&mut shell).is_empty(),
        "Space on the who row changes nothing"
    );
}

#[test]
fn an_empty_page_a_page_of_unready_ones_and_a_failed_change_say_so() {
    let mut empty = view(Vec::new());
    empty.connectors.clear();
    let mut shell = page(empty);
    assert!(shows(&mut shell, "No connectors yet. Add one in Settings, Connectors."));

    let mut unready = view(Vec::new());
    unready.connectors.retain(|connector| connector.name == "board");
    let mut shell = page(unready);
    assert!(shows(
        &mut shell,
        "Not ready yet: Board. Set them up in Settings, Connectors."
    ));
    assert!(!shows(&mut shell, "No connectors yet."));

    let mut on_unready = view(vec![grant("board", true, Who::Both)]);
    on_unready.connectors[2].status = PluginStatus::NeedsKey;
    let mut shell = page(on_unready);
    assert!(shows(&mut shell, "Needs a key  On"));

    let mut shell = page(seeded());
    keys(&mut shell, &[Key::Down, Key::Down, Key::Char(' ')]);
    assert!(matches!(one(&mut shell), ConnectorsJob::Grant { .. }));
    arrive(
        &mut shell,
        ConnectorsEvent::Page(Err("app_plugin_needs_key".into())),
    );
    assert!(said(&shell).contains("The connector was not changed."));
    assert!(said(&shell).contains("This connector needs a key first."));
    assert!(shows(&mut shell, "GitHub"), "a failed change keeps the list");
    keys(&mut shell, &[Key::Char('r')]);
    assert!(reads_project(&one(&mut shell)));
}

#[test]
fn a_failed_read_offers_a_retry_and_the_crumbs_name_the_project() {
    let place = Place::ProjectConnectors {
        id: ProjectId::new(PROJECT).unwrap(),
        name: "Plant shop".into(),
    };
    let mut shell = shell_at(ready(80, 24), place);
    assert!(reads_project(&one(&mut shell)));
    arrive(
        &mut shell,
        ConnectorsEvent::Page(Err("connector_project_unknown".into())),
    );
    assert!(shows(&mut shell, "That project is not set up here any more."));
    assert!(shows(&mut shell, "r Try again"));
    assert!(shows(&mut shell, "Plant shop  /  Connectors"));
    keys(&mut shell, &[Key::Char('r')]);
    assert!(reads_project(&one(&mut shell)));
}

#[test]
fn one_row_per_name_so_built_in_linear_hides_the_owner_s_refused_file_of_its_name() {
    for enabled in [true, false] {
        let mut read = view(vec![grant("linear", enabled, Who::Both)]);
        read.connectors.push(refused_file("linear"));
        let mut shell = page(read);
        let rows = render(&mut shell, 80, 24);
        let switches = rows.iter().filter(|row| row.contains("Linear")).count();
        assert_eq!(switches, 1, "grant {enabled}: one switch for linear: {rows:#?}");
        assert!(!shows(&mut shell, "linear"), "grant {enabled}: no row and no not-ready name");
        assert!(shows(
            &mut shell,
            "Not ready yet: Board. Set them up in Settings, Connectors."
        ));
    }
    let mut broken = view(vec![grant("broken", true, Who::Both)]);
    broken.connectors.push(refused_file("broken"));
    let mut shell = page(broken);
    assert!(
        shows(&mut shell, "Refused  On"),
        "a refused file of its own name keeps its switch, so it can be turned off"
    );
    keys(&mut shell, &[Key::End, Key::Char(' ')]);
    assert!(granted(&one(&mut shell), "broken", false, Who::Both));
}

#[test]
fn an_oauth_only_connector_is_named_under_not_ready_yet_and_an_on_one_keeps_its_switch() {
    let mut read = view(Vec::new());
    read.connectors[1].sign_in = SignIn::NotYet;
    read.connectors[1].secrets.clear();
    let mut shell = page(read.clone());
    assert!(shows(
        &mut shell,
        "Not ready yet: GitHub, Board. Set them up in Settings, Connectors."
    ));
    let rows = render(&mut shell, 80, 24);
    let github = rows.iter().filter(|row| row.contains("GitHub")).count();
    assert_eq!(github, 1, "no switch, only the not-ready name: {rows:#?}");
    read.grants = vec![grant("github", true, Who::Both)];
    let mut shell = page(read);
    assert!(shows(&mut shell, "Unavailable  On"), "an On one can still be turned off");
    keys(&mut shell, &[Key::Down, Key::Char(' ')]);
    assert!(granted(&one(&mut shell), "github", false, Who::Both));
}

/// rv-ds241 T5 F3: an On connector that is not Ready has no "Who can use it" line, as the
/// desktop shows it only while On and Ready, and Left or Right there sends nothing.
#[test]
fn an_on_connector_that_is_not_ready_has_no_who_can_use_it_line() {
    let mut shell = page(view(vec![grant("board", true, Who::Both)]));
    assert!(shows(&mut shell, "Needs review  On"));
    let rows = render(&mut shell, 80, 24);
    let who = rows.iter().filter(|row| row.contains("Who can use it")).count();
    assert_eq!(who, 0, "board is On but needs review: {rows:#?}");
    keys(&mut shell, &[Key::End]);
    keys(&mut shell, &[Key::Right, Key::Left]);
    assert!(requests(&mut shell).is_empty(), "no who row, so nothing to change");
    keys(&mut shell, &[Key::Char(' ')]);
    assert!(granted(&one(&mut shell), "board", false, Who::Both), "it can still be turned off");
    let mut shell = page(view(vec![grant("linear", true, Who::Master)]));
    assert!(shows(&mut shell, "Who can use it"), "a Ready one that is On keeps its who row");
}

/// rv-ds241 T5 nit 3: a failed read wraps its sentence to the page, as Settings does, so
/// the end of a long one is not cut at the edge.
#[test]
fn a_failed_read_wraps_a_long_sentence() {
    let place = Place::ProjectConnectors {
        id: ProjectId::new(PROJECT).unwrap(),
        name: "Plant shop".into(),
    };
    let mut shell = shell_at(ready(80, 24), place);
    assert!(reads_project(&one(&mut shell)));
    arrive(
        &mut shell,
        ConnectorsEvent::Page(Err("app_plugin_folder_unsafe".into())),
    );
    assert!(shows(&mut shell, "Your plugins folder isn't private to you"));
    assert!(shows(&mut shell, "yours alone and try again."), "the end is on the next line");
}

fn allowed(project: &str, connectors: &[&str]) -> AllowedWrites {
    AllowedWrites {
        project: project.into(),
        connectors: connectors.iter().map(|name| (*name).to_owned()).collect(),
    }
}

fn reads_writes(job: &ConnectorsJob) -> bool {
    matches!(job, ConnectorsJob::Writes { project } if project == PROJECT)
}

/// The page under Ask once per project and connector, read with `grants`; the allowed
/// writes read goes out after the page lands and has not arrived yet.
fn ask_once(grants: Vec<Grant>) -> Shell {
    let mut read = view(grants);
    read.settings.write = WritePolicy::AskOncePerProject;
    let mut shell = page(read);
    assert!(
        reads_writes(&one(&mut shell)),
        "the allowed writes are read after the page"
    );
    shell
}

#[test]
fn allowed_writes_show_only_under_ask_once_and_x_revokes_one_at_a_time() {
    let mut every = page(view(vec![]));
    assert!(
        requests(&mut every).is_empty(),
        "no writes read outside ask once"
    );
    assert!(!shows(&mut every, "Allowed writes"));
    let mut shell = ask_once(vec![grant("linear", true, Who::Both)]);
    arrive(
        &mut shell,
        ConnectorsEvent::Writes(Ok(allowed(PROJECT, &["linear", "gone"]))),
    );
    assert!(shows(&mut shell, "Allowed writes"));
    assert!(shows(
        &mut shell,
        "Connectors that may change things in this project without asking again."
    ));
    assert!(
        shows(&mut shell, "gone"),
        "a connector no longer listed shows its name"
    );
    keys(&mut shell, &[Key::End]);
    assert!(shows(&mut shell, "x Revoke"));
    keys(&mut shell, &[Key::Char('x')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::Revoke { project, connector } if project == PROJECT && connector == "gone"
    ));
    keys(&mut shell, &[Key::Up, Key::Char('x'), Key::Char(' ')]);
    assert!(requests(&mut shell).is_empty(), "one request at a time");
    arrive(
        &mut shell,
        ConnectorsEvent::Writes(Ok(allowed("beta", &["notion"]))),
    );
    assert!(
        !shows(&mut shell, "notion"),
        "another project's writes are not this page's"
    );
    arrive(
        &mut shell,
        ConnectorsEvent::Writes(Ok(allowed(PROJECT, &["linear"]))),
    );
    assert!(said(&shell).contains("gone asks again before its next write."));
    assert!(
        !render(&mut shell, 80, 24)
            .iter()
            .any(|row| row.trim().trim_start_matches("> ") == "gone"),
        "the revoked row is gone, the toast names it"
    );
    keys(&mut shell, &[Key::End, Key::Char('x')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::Revoke { connector, .. } if connector == "linear"
    ));
    arrive(
        &mut shell,
        ConnectorsEvent::Writes(Err("connectors_unavailable".into())),
    );
    assert!(said(&shell).contains("The connector was not changed."));
    keys(&mut shell, &[Key::Char('x')]);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::Revoke { connector, .. } if connector == "linear"
    ));
    arrive(
        &mut shell,
        ConnectorsEvent::Writes(Ok(allowed(PROJECT, &[]))),
    );
    assert!(shows(
        &mut shell,
        "No connector may change things here without asking."
    ));
}

#[test]
fn allowed_writes_say_when_they_are_read_and_when_the_read_failed() {
    let mut shell = ask_once(Vec::new());
    assert!(shows(&mut shell, "Reading allowed writes"));
    arrive(
        &mut shell,
        ConnectorsEvent::Writes(Err("connectors_unavailable".into())),
    );
    assert!(shows(&mut shell, "Couldn't load the allowed writes."));
    keys(&mut shell, &[Key::Char('r')]);
    assert!(
        reads_project(&one(&mut shell)),
        "r reads the page again first"
    );
}

#[test]
fn a_change_that_lands_under_ask_once_reads_the_allowed_writes_and_another_policy_forgets_them() {
    let mut shell = page(view(vec![grant("linear", true, Who::Both)]));
    keys(&mut shell, &[Key::Char(' ')]);
    assert!(granted(&one(&mut shell), "linear", false, Who::Both));
    let mut once = view(vec![grant("linear", false, Who::Both)]);
    once.settings.write = WritePolicy::AskOncePerProject;
    arrive(&mut shell, ConnectorsEvent::Page(Ok(once.clone())));
    assert!(
        reads_writes(&one(&mut shell)),
        "the block's read goes out once the page shows it"
    );
    arrive(
        &mut shell,
        ConnectorsEvent::Writes(Ok(allowed(PROJECT, &["linear"]))),
    );
    assert!(shows(&mut shell, "Allowed writes"));
    keys(&mut shell, &[Key::Char('r')]);
    assert!(reads_project(&one(&mut shell)));
    arrive(
        &mut shell,
        ConnectorsEvent::Page(Ok(view(vec![grant("linear", false, Who::Both)]))),
    );
    assert!(requests(&mut shell).is_empty());
    assert!(!shows(&mut shell, "Allowed writes"));
    keys(&mut shell, &[Key::Char('r')]);
    assert!(reads_project(&one(&mut shell)));
    arrive(&mut shell, ConnectorsEvent::Page(Ok(once)));
    assert!(reads_writes(&one(&mut shell)));
    assert!(
        shows(&mut shell, "Reading allowed writes"),
        "a list read under the last policy is not shown again"
    );
}
