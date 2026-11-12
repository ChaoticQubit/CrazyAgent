use super::*;
use crate::app_plugins::Problem;
use crate::semantic::Progress;
use crate::tui::client::OwnerEvent;
use crate::tui::screens::open;
use crate::tui::screens::settings_accounts::tests::{ctx, render};
use crate::tui::seam::Request;
use crate::tui::shell::{Shell, targets};

const SIZE: u64 = 125_729_604;

fn option(
    name: &str,
    title: &str,
    runs: RunsView,
    usable: bool,
    reason: Option<&str>,
) -> SearchOption {
    SearchOption {
        name: name.into(),
        title: title.into(),
        sends: match runs {
            RunsView::Http { .. } => vec!["Session text".into(), "Memories".into()],
            RunsView::Local { .. } => Vec::new(),
        },
        runs,
        usable,
        reason: reason.map(Into::into),
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

fn view() -> SearchView {
    SearchView {
        version: 4,
        choice: SearchChoice::Plugin {
            name: "local".into(),
        },
        options: vec![
            option(
                "local",
                "Built-in model",
                RunsView::Local {
                    runtime: "static-retrieval-mrl-en-v1".into(),
                    download_bytes: SIZE,
                },
                true,
                None,
            ),
            option(
                "dock-embed",
                "Dock embeddings",
                http("http://LocalHost:11434/api/embed", "localhost", true),
                true,
                None,
            ),
            option(
                "voyage",
                "Voyage embeddings",
                http(
                    "https://api.voyageai.com/v1/embeddings",
                    "api.voyageai.com",
                    false,
                ),
                false,
                Some("Set its key in App plugins first."),
            ),
        ],
        model: ModelState::Ready { size_bytes: SIZE },
        progress: Progress {
            embedded: 120,
            total: 480,
        },
    }
}

fn requests(shell: &mut Shell) -> Vec<AppPluginsJob> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Settings(job) => match *job {
                SettingsJob::AppPlugins(job) => Some(*job),
                other => panic!("the page sent {other:?}"),
            },
            _ => None,
        })
        .collect()
}

fn one(shell: &mut Shell) -> AppPluginsJob {
    let mut sent = requests(shell);
    assert_eq!(sent.len(), 1, "one request at a time");
    sent.remove(0)
}

fn arrive(shell: &mut Shell, event: AppPluginsEvent) {
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::AppPlugins(Box::new(event)),
    )))));
}

fn page(read: SearchView) -> Shell {
    let mut shell = Shell::with(ctx(100, 40), Box::new(SearchPage::new()));
    assert!(matches!(one(&mut shell), AppPluginsJob::Search));
    arrive(&mut shell, AppPluginsEvent::Search(Ok(read)));
    shell
}

fn shows(shell: &mut Shell, text: &str) -> bool {
    render(shell, 100, 40).iter().any(|row| row.contains(text))
}

fn said(shell: &Shell) -> String {
    shell
        .ctx
        .message()
        .map(|message| format!("{} {}", message.lead, message.rest))
        .unwrap_or_default()
}

#[test]
fn s1_s2_s10_full_text_and_every_plugin_are_offered_with_the_saved_one_marked() {
    let mut shell = page(view());
    assert!(shows(&mut shell, "( ) Full text only"));
    assert!(shows(
        &mut shell,
        "Exact words only. Nothing is downloaded or sent."
    ));
    assert!(shows(&mut shell, "(*) Built-in model"));
    assert!(shows(&mut shell, "( ) Dock embeddings"));
    assert!(shows(
        &mut shell,
        "Reaches localhost (local, not encrypted)"
    ));
    assert!(shows(&mut shell, "( ) Voyage embeddings"));
    assert!(shows(&mut shell, "Set its key in App plugins first."));

    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::SetSearch {
            choice: SearchChoice::FullText,
            confirm_host: None,
            version: 4
        }
    ));
    let mut saved = view();
    saved.choice = SearchChoice::FullText;
    saved.version = 5;
    arrive(
        &mut shell,
        AppPluginsEvent::SearchSaved(Ok(SearchSaved::Saved(Box::new(saved)))),
    );
    assert!(shows(&mut shell, "(*) Full text only"));
    assert!(said(&shell).contains("Search setting saved."));

    shell.key(Key::Down);
    shell.key(Key::Enter);
    assert!(matches!(
        one(&mut shell),
        AppPluginsJob::SetSearch { choice: SearchChoice::Plugin { ref name }, confirm_host: None, version: 5 } if name == "local"
    ));
}

#[test]
fn s10_an_unusable_plugin_is_not_chosen_and_says_why() {
    let mut shell = page(view());
    for _ in 0..3 {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    assert!(requests(&mut shell).is_empty());
    assert!(said(&shell).contains("Set its key in App plugins first."));
}

#[test]
fn s3_s4_an_http_plugin_asks_first_naming_its_host_and_what_leaves_the_computer() {
    let mut shell = page(view());
    shell.key(Key::Down);
    shell.key(Key::Down);
    shell.key(Key::Enter);
    assert!(
        requests(&mut shell).is_empty(),
        "nothing is saved before the confirm"
    );
    assert!(shows(&mut shell, "Send search text to localhost?"));
    assert!(shows(
        &mut shell,
        "Once chosen, CrazyAgent sends these to localhost in the"
    ));
    assert!(shows(&mut shell, "- Session text"));
    assert!(shows(&mut shell, "- Memories"));
    shell.key(Key::Enter);
    assert!(requests(&mut shell).is_empty(), "only y confirms");
    shell.key(Key::Esc);
    assert!(!shows(&mut shell, "Send search text to localhost?"));
    assert_eq!(shell.place(), Some(Place::SearchSettings));
    shell.key(Key::Enter);
    shell.key(Key::Char('y'));
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::SetSearch { choice: SearchChoice::Plugin { ref name }, confirm_host: Some(ref host), version: 4 }
            if name == "dock-embed" && host == "localhost"
    ));
    arrive(
        &mut shell,
        AppPluginsEvent::SearchSaved(Err("search_confirm_required".into())),
    );
    assert!(shows(
        &mut shell,
        "This plugin sends search text to a host you haven't confirmed."
    ));
}

#[test]
fn s5_s6_a_missing_model_shows_its_size_and_d_downloads_after_y_with_confirm_bytes() {
    let mut read = view();
    read.model = ModelState::NotDownloaded { size_bytes: SIZE };
    let mut shell = page(read);
    assert!(shows(
        &mut shell,
        "Not on this computer. A 126 MB download, once; it then works offline."
    ));
    shell.key(Key::Char('d'));
    assert!(requests(&mut shell).is_empty());
    assert!(shows(&mut shell, "Download the built-in model?"));
    assert!(shows(
        &mut shell,
        "126 MB, once. CrazyAgent checks it before use;"
    ));
    shell.key(Key::Char('y'));
    let job = one(&mut shell);
    assert!(matches!(
        job,
        AppPluginsJob::Download {
            confirm_bytes: SIZE
        }
    ));
    let mut running = view();
    running.model = ModelState::Downloading {
        done_bytes: 63_000_000,
        size_bytes: SIZE,
    };
    arrive(&mut shell, AppPluginsEvent::Search(Ok(running)));
    assert!(!shows(&mut shell, "Download the built-in model?"));
    assert!(shows(&mut shell, "Downloading: 63 MB of 126 MB"));
}

#[test]
fn s7_a_running_download_shows_how_far_it_got_and_c_cancels_it() {
    let mut read = view();
    read.model = ModelState::Downloading {
        done_bytes: 63_000_000,
        size_bytes: SIZE,
    };
    let mut shell = page(read);
    assert!(shows(&mut shell, "Downloading: 63 MB of 126 MB"));
    assert!(shows(&mut shell, "c Cancel download"));
    shell.key(Key::Char('c'));
    assert!(matches!(one(&mut shell), AppPluginsJob::CancelDownload));
}

#[test]
fn s8_s9_a_failed_download_offers_d_again_and_a_ready_model_x_deletes_after_y() {
    let mut read = view();
    read.model = ModelState::Failed {
        problem: Problem {
            code: "search_model_hash_mismatch".into(),
            line: None,
            field: None,
            message: "The download didn't match its checksum.".into(),
        },
        size_bytes: SIZE,
    };
    let mut shell = page(read);
    assert!(shows(
        &mut shell,
        "The download didn't match its checksum. (search_model_hash_mismatch)"
    ));
    assert!(shows(&mut shell, "d Download again"));
    shell.key(Key::Char('d'));
    assert!(shows(&mut shell, "Download the built-in model?"));
    shell.key(Key::Esc);

    let mut shell = page(view());
    assert!(shows(&mut shell, "Ready: 126 MB on this computer."));
    shell.key(Key::Char('d'));
    assert!(
        !shows(&mut shell, "Download the built-in model?"),
        "a ready model is not downloaded again"
    );
    shell.key(Key::Char('x'));
    assert!(shows(&mut shell, "Delete the built-in model?"));
    assert!(shows(
        &mut shell,
        "Frees 126 MB. Search with the built-in model stops until you"
    ));
    shell.key(Key::Char('y'));
    assert!(matches!(one(&mut shell), AppPluginsJob::DeleteModel));
}

#[test]
fn s11_s12_embedding_progress_shows_for_a_plugin_choice_and_a_stale_save_reads_again() {
    let mut shell = page(view());
    assert!(shows(
        &mut shell,
        "Embedded 120 of 480 sessions and memories."
    ));
    shell.key(Key::Enter);
    let _ = one(&mut shell);
    arrive(
        &mut shell,
        AppPluginsEvent::SearchSaved(Ok(SearchSaved::Stale(9))),
    );
    assert!(shows(&mut shell, SEARCH_STALE));
    assert!(
        matches!(one(&mut shell), AppPluginsJob::Search),
        "a stale save reads the page again"
    );

    let mut full = view();
    full.choice = SearchChoice::FullText;
    let mut shell = page(full);
    assert!(!shows(&mut shell, "Embedded"));
}

#[test]
fn pud_m1_the_page_reads_again_on_refresh_only_while_something_moves() {
    let mut read = view();
    read.model = ModelState::Downloading {
        done_bytes: 1_000_000,
        size_bytes: SIZE,
    };
    read.choice = SearchChoice::FullText;
    let mut shell = page(read);
    shell.refresh();
    assert!(
        matches!(one(&mut shell), AppPluginsJob::Search),
        "a running download is read again"
    );

    let mut embedding = view();
    embedding.progress = Progress {
        embedded: 120,
        total: 480,
    };
    arrive(&mut shell, AppPluginsEvent::Search(Ok(embedding)));
    shell.refresh();
    assert!(
        matches!(one(&mut shell), AppPluginsJob::Search),
        "a usable chosen plugin still embedding"
    );

    let mut done = view();
    done.progress = Progress {
        embedded: 480,
        total: 480,
    };
    arrive(&mut shell, AppPluginsEvent::Search(Ok(done)));
    shell.refresh();
    assert!(
        requests(&mut shell).is_empty(),
        "nothing moves, nothing is read"
    );

    let mut stuck = view();
    stuck.choice = SearchChoice::Plugin {
        name: "voyage".into(),
    };
    let mut shell = page(stuck);
    shell.refresh();
    assert!(
        requests(&mut shell).is_empty(),
        "an unusable plugin embeds nothing"
    );
    assert!(polling(&SearchView {
        model: ModelState::Downloading {
            done_bytes: 0,
            size_bytes: SIZE
        },
        ..view()
    }));
}

#[test]
fn search_opens_from_go_to_and_a_refused_read_offers_r() {
    let mut context = ctx(80, 24);
    assert!(
        targets(&context)
            .iter()
            .any(|target| target.label == "Settings / Search"
                && target.kind == "Settings"
                && target.place == Place::SearchSettings)
    );
    let mut screen = open(&Place::SearchSettings, &mut context);
    assert_eq!(screen.crumbs(), ["Settings", "Search"]);
    assert!(matches!(screen.key(&Key::Esc, &mut context), Flow::Ignored));

    let mut shell = Shell::with(ctx(100, 40), Box::new(SearchPage::new()));
    assert!(matches!(one(&mut shell), AppPluginsJob::Search));
    arrive(
        &mut shell,
        AppPluginsEvent::Search(Err("app_plugins_unavailable".into())),
    );
    assert!(shows(
        &mut shell,
        "App plugins aren't available in this engine yet."
    ));
    shell.key(Key::Char('r'));
    assert!(matches!(one(&mut shell), AppPluginsJob::Search));
    assert_eq!(
        host_of("http://LocalHost:11434/x"),
        Some("localhost".into())
    );
    assert_eq!(host_of("not an address"), None);
}
