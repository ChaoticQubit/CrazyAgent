use super::*;
use crate::harnesses::{Search, catalog};
use crate::tui::client::OwnerEvent;
use crate::tui::screens::settings_accounts::tests::{dataset, fixture, list_ctx, render};
use crate::tui::seam::Request;
use crate::tui::shell::Shell;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Found {
    _root: tempfile::TempDir,
    entries: Vec<HarnessEntry>,
    codex: PathBuf,
}

fn found() -> Found {
    let root = tempfile::Builder::new()
        .prefix("ca-th-")
        .tempdir_in("/private/tmp")
        .unwrap();
    for name in ["codex", "claude"] {
        let path = root.path().join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let entries = catalog(&Search::new(
        vec![root.path().to_path_buf()],
        None,
        Vec::new(),
    ));
    let codex = root.path().join("codex");
    Found {
        _root: root,
        entries,
        codex,
    }
}

fn harness_ctx(width: u16, height: u16) -> Ctx {
    let mut ctx = list_ctx(width, height, dataset(None).1);
    ctx.store.harnesses = Load::Ready((2, 9));
    ctx
}

fn put(row: &str, column: usize, text: &str, room: usize) -> String {
    let mut cells: Vec<char> = row.chars().collect();
    let mut value: Vec<char> = text.chars().collect();
    value.resize(room, ' ');
    cells.splice(column..column + room, value);
    cells.into_iter().collect()
}

fn engine_table(rows: &mut [String], first: usize, control: usize, works: usize, room: usize) {
    let words = [
        "Any account",
        "Subscription, API key, Local model",
        "Any account",
        "Subscription, API key",
        "Subscription, API key",
        "Any API key or URL",
        "Subscription",
        "Subscription, API key, Local model",
        "Any account",
    ];
    for (index, word) in words.iter().enumerate() {
        let row = &mut rows[first + index];
        *row = put(row, works, &cut(word, room, "…"), room);
        if index == 5 || index == 7 {
            *row = put(row, control, "◧ Limited control", 17);
        }
    }
}

#[test]
fn harness_list_frames_match_the_design() {
    let found = found();
    let nine: Vec<HarnessEntry> = found.entries.iter().take(9).cloned().collect();
    let path = found.codex.display().to_string();

    let mut shell = Shell::with(harness_ctx(80, 24), Box::new(Harnesses::with(nine.clone())));
    let mut expected = fixture("harnesses-80", &[]);
    engine_table(&mut expected, 6, 35, 54, 24);
    expected[17] = put(&expected[17], 15, &path, 63);
    expected[20] = " ".repeat(80);
    expected[21] = " ".repeat(80);
    assert_eq!(render(&mut shell, 80, 24), expected);

    let mut shell = Shell::with(harness_ctx(120, 36), Box::new(Harnesses::with(nine)));
    let mut expected = fixture("harnesses-120", &[]);
    engine_table(&mut expected, 8, 37, 56, 62);
    expected[19] = put(&expected[19], 15, &path, 103);
    expected[20] = expected[21].clone();
    for row in 21..=23 {
        expected[row] = " ".repeat(120);
    }
    assert_eq!(render(&mut shell, 120, 36), expected);
}

#[test]
fn a_harness_that_is_not_installed_says_how_to_add_it() {
    let found = found();
    let nine: Vec<HarnessEntry> = found.entries.iter().take(9).cloned().collect();
    let mut shell = Shell::with(harness_ctx(80, 24), Box::new(Harnesses::with(nine)));
    for _ in 0..4 {
        shell.key(Key::Down);
    }
    let mut expected = fixture("harness-limited-80", &[]);
    engine_table(&mut expected, 6, 35, 54, 24);
    expected[17] = format!(
        "{:<80}",
        "  Limited control: tools are allowed or denied only through Cursor's"
    );
    expected[18] = format!("{:<80}", "  permission file.");
    assert_eq!(render(&mut shell, 80, 24), expected);
}

#[test]
fn every_engine_harness_scrolls_in_a_short_window() {
    let found = found();
    assert_eq!(found.entries.len(), crate::harnesses::specs().len());
    let mut shell = Shell::with(
        harness_ctx(80, 24),
        Box::new(Harnesses::with(found.entries.clone())),
    );
    shell.key(Key::End);
    let rows = render(&mut shell, 80, 24);
    let last = &found.entries[found.entries.len() - 1].name;
    assert!(
        rows[14].contains(&format!("> {}", cut(last, 12, "…"))),
        "{}",
        rows[14]
    );
    assert!(rows[4].contains(&format!("2 of {} installed", found.entries.len())));
    assert!(
        rows[6..15]
            .iter()
            .all(|row| row.chars().nth(77) != Some(' ')),
        "scroll bar shows"
    );
}

#[test]
fn check_again_asks_the_engine_and_the_reply_updates_the_counts() {
    let found = found();
    let mut ctx = harness_ctx(120, 36);
    ctx.store.harnesses = Load::NotReady;
    let mut screen = Harnesses::new();
    screen.shown(&mut ctx);
    assert!(matches!(ctx.next_request(), Some(Request::Settings(_))));
    assert!(screen.busy(&ctx));
    screen.key(&Key::Char('r'), &mut ctx);
    assert_eq!(ctx.queued(), 0, "one check at a time");
    let reply = Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Harnesses(Ok(found.entries.clone())),
    ))));
    screen.arrive(&reply, &mut ctx);
    assert!(!screen.busy(&ctx));
    assert_eq!(ctx.store.harnesses, Load::Ready((2, found.entries.len())));
    screen.key(&Key::Char('r'), &mut ctx);
    assert!(matches!(ctx.next_request(), Some(Request::Settings(_))));
    let failed = Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Harnesses(Err("controller_pending_intent".into())),
    ))));
    screen.arrive(&failed, &mut ctx);
    assert_eq!(ctx.message().unwrap().lead, "Could not check harnesses.");
    assert_eq!(
        screen.entries.len(),
        found.entries.len(),
        "a failed check keeps the list"
    );
}

#[test]
fn words_follow_the_engine_catalog() {
    let found = found();
    let codex = &found.entries[0];
    assert_eq!(
        (status_word(codex), control_word(codex)),
        ("Ready", "Full control")
    );
    let mut cursor = found
        .entries
        .iter()
        .find(|entry| entry.id == "cursor-agent")
        .unwrap()
        .clone();
    assert_eq!(
        (status_word(&cursor), control_word(&cursor)),
        ("Not installed", "Limited control")
    );
    cursor.executable = Some(found.codex.clone());
    cursor.status = HarnessStatus::Limited {
        reason: "installed, but CrazyAgent cannot start it yet".into(),
    };
    assert_eq!(status_word(&cursor), "Can't start");
    assert_eq!(
        control_sentence(&cursor),
        "Limited control: installed, but CrazyAgent cannot start it yet; tools are allowed or denied only through Cursor's permission file."
    );
    let opencode = found
        .entries
        .iter()
        .find(|entry| entry.id == "opencode")
        .unwrap();
    assert_eq!(control_word(opencode), "Unknown");
    assert_eq!(works_with(&[]), "Not known yet");
    assert_eq!(works_with(&[AccountKind::Local]), "Local model");
}
