use super::text::{pad, safe};
use super::wire::{Refused, ask, unreadable};
use super::{Client, Key, Theme, put};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use serde::Deserialize;
use serde_json::json;
use std::cell::Cell;
use trackwright_core::model::OpKind;
use trackwright_core::runs::{
    IntegrityRef, PolicyRef, ResultView, RunState, SourceRef, Totals, Verdict,
};
use trackwright_core::tests_model::RESULTS_PER_RUN_MAX;

const LABEL_CELLS: usize = 13;
const OUTCOME_CELLS: usize = 9;
const CASE_CELLS: usize = 14;
const DIGEST_SHOWN: usize = 12;
const PAGES_MAX: usize = RESULTS_PER_RUN_MAX + 1;

#[derive(Clone, Copy)]
enum Role {
    Heading,
    Muted,
    Text,
}

#[derive(Debug, Deserialize)]
struct Header {
    id: String,
    title: String,
    #[serde(default)]
    batch: Option<String>,
    state: RunState,
    #[serde(default)]
    verdict: Option<Verdict>,
    #[serde(default)]
    reason: Option<String>,
    recorded_by: String,
    totals: Totals,
}

#[derive(Debug, Deserialize)]
struct Shown {
    run: Header,
    #[serde(default)]
    source: Option<SourceRef>,
    #[serde(default)]
    source_cut: bool,
    #[serde(default)]
    policy: Option<PolicyRef>,
    #[serde(default)]
    policy_cut: bool,
    #[serde(default)]
    integrity: Option<IntegrityRef>,
    #[serde(default)]
    integrity_cut: bool,
    #[serde(default)]
    unlinked_total: u32,
    #[serde(default)]
    results: Vec<ResultView>,
}

#[derive(Debug, Deserialize)]
struct More {
    #[serde(default)]
    results: Vec<ResultView>,
}

/// One run's detail: its state, verdict and counts, the binding block (the source and
/// check policy it covers), the integrity block and every result, scrolled as one column.
pub struct RunDetail {
    shown: Shown,
    scroll: usize,
    bottom: Cell<usize>,
}

impl RunDetail {
    /// Reads one run with `run_show`, following the cursor until every result is in; each
    /// later page must add results and move the cursor. Inputs: the run id and the client.
    /// Output: the detail, or the refusal.
    pub fn load(id: &str, client: &mut dyn Client) -> Result<Self, Refused> {
        let (mut shown, mut cursor) = ask::<Shown>(client, OpKind::RunShow, json!({ "id": id }))?;
        let mut pages = 1;
        while let Some(at) = cursor {
            if pages == PAGES_MAX {
                return Err(unreadable(OpKind::RunShow));
            }
            let input = json!({ "id": id, "cursor": at });
            let (more, next) = ask::<More>(client, OpKind::RunShow, input)?;
            if more.results.is_empty() || next.as_deref() == Some(at.as_str()) {
                return Err(unreadable(OpKind::RunShow));
            }
            shown.results.extend(more.results);
            cursor = next;
            pages += 1;
        }
        assert!(pages <= PAGES_MAX);
        if shown.run.id != id {
            return Err(unreadable(OpKind::RunShow));
        }
        Ok(Self {
            shown,
            scroll: 0,
            bottom: Cell::new(0),
        })
    }

    /// The run's id. Output: the id as the reply gave it.
    pub fn id(&self) -> &str {
        &self.shown.run.id
    }

    /// Acts on a key: Up, Down, PageUp and PageDown scroll, no further than the last
    /// drawn line. Input: the key. Output: true when used.
    pub fn key(&mut self, key: Key) -> bool {
        let bottom = self.bottom.get();
        match key {
            Key::Up => self.scroll = self.scroll.saturating_sub(1),
            Key::Down => self.scroll = (self.scroll + 1).min(bottom),
            Key::PageUp => self.scroll = self.scroll.saturating_sub(10),
            Key::PageDown => self.scroll = (self.scroll + 10).min(bottom),
            _ => return false,
        }
        true
    }

    /// Draws the lines that fit from the scroll position. Inputs: the area, the buffer and
    /// the theme.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let lines = self.lines(usize::from(area.width));
        let visible = usize::from(area.height);
        self.bottom.set(lines.len().saturating_sub(visible));
        let start = self.scroll.min(self.bottom.get());
        for (row, (text, role)) in lines.iter().skip(start).take(visible).enumerate() {
            let style: Style = match role {
                Role::Heading => theme.heading,
                Role::Muted => theme.muted,
                Role::Text => theme.text,
            };
            let row = u16::try_from(row).expect("visible rows fit a u16");
            put(area, buf, row, text, style);
        }
    }

    fn lines(&self, limit: usize) -> Vec<(String, Role)> {
        let shown = &self.shown;
        let run = &shown.run;
        let mut lines = vec![(
            format!(" {}  {}", safe(&run.id), safe(&run.title)),
            Role::Heading,
        )];
        let mut facts = vec![
            state_label(run.state).to_owned(),
            run.verdict.map_or("no verdict", Verdict::as_str).to_owned(),
            format!("{} of {} passed", run.totals.passed, run.totals.total),
        ];
        facts.extend(
            run.batch
                .as_deref()
                .map(|batch| format!("batch {}", safe(batch))),
        );
        facts.push(format!("by {}", safe(&run.recorded_by)));
        lines.push((format!(" {}", facts.join("  ")), Role::Muted));
        if let Some(reason) = &run.reason {
            lines.push((format!(" Reason: {}", safe(reason)), Role::Muted));
        }
        lines.push((String::new(), Role::Text));
        lines.push((" Binding".to_owned(), Role::Heading));
        lines.extend(source_lines(shown.source.as_ref(), shown.source_cut));
        lines.extend(policy_lines(shown.policy.as_ref(), shown.policy_cut));
        lines.push((String::new(), Role::Text));
        lines.push((" Integrity".to_owned(), Role::Heading));
        lines.extend(integrity_lines(
            shown.integrity.as_ref(),
            shown.integrity_cut,
        ));
        lines.push((String::new(), Role::Text));
        lines.push((
            format!(
                " Results  {}  unlinked {}",
                shown.results.len(),
                shown.unlinked_total
            ),
            Role::Heading,
        ));
        let name_cells = limit.saturating_sub(3 + OUTCOME_CELLS + CASE_CELLS);
        for result in &shown.results {
            let case = result
                .case
                .as_ref()
                .map_or_else(|| "unlinked".to_owned(), ToString::to_string);
            lines.push((
                format!(
                    "   {}{}{}",
                    pad(result.outcome.as_str(), OUTCOME_CELLS),
                    pad(&safe(&result.name), name_cells),
                    pad(&case, CASE_CELLS),
                ),
                Role::Text,
            ));
        }
        lines
    }
}

fn state_label(state: RunState) -> &'static str {
    match state {
        RunState::Running => "Running",
        RunState::Finished => "Finished",
        RunState::Interrupted => "Interrupted",
    }
}

fn fact(label: &str, value: &str) -> (String, Role) {
    (format!("   {}{value}", pad(label, LABEL_CELLS)), Role::Text)
}

fn short(digest: &str) -> String {
    safe(digest.get(..DIGEST_SHOWN).unwrap_or(digest))
}

fn cut_line(label: &str) -> Vec<(String, Role)> {
    vec![fact(
        label,
        "cut from this reply; a larger reply bound shows it",
    )]
}

fn source_lines(source: Option<&SourceRef>, cut: bool) -> Vec<(String, Role)> {
    match (source, cut) {
        (_, true) => cut_line("Source"),
        (None, false) => vec![fact("Source", "not in the reply")],
        (Some(SourceRef::Declared { reference }), false) => {
            vec![fact("Source", &format!("declared {}", safe(reference)))]
        }
        (
            Some(SourceRef::Git {
                repository,
                commit,
                tree,
                base,
                change_set,
            }),
            false,
        ) => {
            let mut lines = vec![
                fact("Source", &format!("git {}", safe(repository))),
                fact(
                    "Commit",
                    &format!("{}  tree {}", short(commit), short(tree)),
                ),
            ];
            lines.extend(base.as_deref().map(|base| fact("Base", &short(base))));
            lines.extend(change_set.as_deref().map(|set| fact("Change", &safe(set))));
            lines
        }
    }
}

fn policy_lines(policy: Option<&PolicyRef>, cut: bool) -> Vec<(String, Role)> {
    let policy = match (policy, cut) {
        (_, true) => return cut_line("Policy"),
        (None, false) => return vec![fact("Policy", "not in the reply")],
        (Some(policy), false) => policy,
    };
    let command = if policy.argv.is_empty() {
        "none".to_owned()
    } else {
        let quoted: Vec<String> = policy.argv.iter().map(|arg| quoted(arg)).collect();
        safe(&quoted.join(" "))
    };
    let mut toolchain = policy.toolchain.family.as_str().to_owned();
    if !policy.toolchain.version.is_empty() {
        toolchain = format!("{toolchain} {}", safe(&policy.toolchain.version));
    }
    let sandbox = format!(
        "{}  network {}  timeout {} s",
        short(&policy.sandbox),
        policy.network.as_str(),
        policy.timeout_s
    );
    vec![
        fact(
            "Check",
            &format!("{}  kind {}", safe(&policy.check), policy.kind.as_str()),
        ),
        fact("Command", &command),
        fact("Profile", &short(&policy.profile)),
        fact("Toolchain", &toolchain),
        fact("Sandbox", &sandbox),
        fact("Environment", &short(&policy.environment)),
    ]
}

fn quoted(arg: &str) -> String {
    let plain = !arg.is_empty()
        && !arg
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, '\'' | '"' | '\\'));
    if plain {
        return arg.to_owned();
    }
    let quoted = format!("'{}'", arg.replace('\'', r"'\''"));
    assert!(quoted.len() >= arg.len() + 2);
    quoted
}

fn integrity_lines(integrity: Option<&IntegrityRef>, cut: bool) -> Vec<(String, Role)> {
    let integrity = match (integrity, cut) {
        (_, true) => return cut_line("Integrity"),
        (None, false) => return vec![("   none recorded".to_owned(), Role::Muted)],
        (Some(integrity), false) => integrity,
    };
    let build = if integrity.build_ok { "ok" } else { "failed" };
    let holdouts = if integrity.holdouts_ran {
        "ran"
    } else {
        "not run"
    };
    let inventory = &integrity.inventory;
    let mut lines = vec![
        fact(
            "Executed",
            &format!("{}  build {build}  holdouts {holdouts}", integrity.executed),
        ),
        fact(
            "Removed",
            &format!(
                "{}  new skips {}  protected {}",
                inventory.removed_total,
                integrity.new_skips.len(),
                integrity.protected_touched.len()
            ),
        ),
    ];
    lines.extend(
        inventory
            .baseline_run
            .as_ref()
            .map(|run| fact("Baseline", &run.to_string())),
    );
    lines
}
