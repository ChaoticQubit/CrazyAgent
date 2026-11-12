use super::{Cx, Failure, Outcome};
use crate::coverage::{self, CoverageOf, CoverageQuery, Finished};
use crate::host::Host;
use crate::model::{ShortId, codes};
use crate::reply::Page;
use crate::runs::{
    self, ResultView, RunCancel, RunEnd, RunFinish, RunList, RunOutcome, RunRecord, RunShow,
    RunStart, RunView, Sighted, StopReason, Verdict,
};
use crate::tests_model::{
    CaseKind, CaseProtectedVisibility, CaseStatus, CaseView, CriterionRef, LIST_ROWS_MAX,
    PlanItemRef, PlanStatus, StepsView, TestId, TestObject, sees_protected,
};
use crate::tests_store::{self, CaseChange, CaseFilter, NewCase, PlanFields};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Lists a project's cases by number within the page budget, each latest outcome as the
/// host's run sight lets the actor read it. Input: the context. Output: `cases` and a
/// `case:<n>` cursor while more may follow.
pub(super) fn case_list<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let project = cx.project();
    let after = match cursor(cx, "case", 1)? {
        Some(number) => Some(TestId::new(project.clone(), TestObject::Case, number)?),
        None => None,
    };
    let filter = CaseFilter {
        kind: cx.args.get::<CaseKind>("kind"),
        status: cx.args.get::<CaseStatus>("status"),
        set: cx.args.get::<&TestId>("set").cloned(),
        unlinked: cx.args.get::<bool>("unlinked").unwrap_or(false),
        after,
    };
    let rows = tests_store::case_list_as(cx.tx, cx.host, cx.actor, project, &filter)?;
    assert!(rows.len() <= LIST_ROWS_MAX);
    let mut page = Page::new(cx.data_bytes_max);
    let taken = fill(&mut page, "cases", &rows);
    assert!(
        rows.is_empty() || taken > 0,
        "one case row always fits a page"
    );
    let more = taken < rows.len() || rows.len() == LIST_ROWS_MAX;
    let next = more.then(|| format!("case:{}", rows[taken - 1].id.number()));
    Ok(done(page.into_value(), next))
}

/// Lists a project's plans by number within the page budget. Input: the context. Output:
/// `plans` and a `plan:<n>` cursor while more may follow.
pub(super) fn plan_list<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let project = cx.project();
    let after = match cursor(cx, "plan", 1)? {
        Some(number) => Some(TestId::new(project.clone(), TestObject::Plan, number)?),
        None => None,
    };
    let status = cx.args.get::<PlanStatus>("status");
    let rows = tests_store::plan_list(cx.tx, project, status, after.as_ref())?;
    assert!(rows.len() <= LIST_ROWS_MAX);
    let mut page = Page::new(cx.data_bytes_max);
    let taken = fill(&mut page, "plans", &rows);
    assert!(
        rows.is_empty() || taken > 0,
        "one plan row always fits a page"
    );
    let more = taken < rows.len() || rows.len() == LIST_ROWS_MAX;
    let next = more.then(|| format!("plan:{}", rows[taken - 1].id.number()));
    Ok(done(page.into_value(), next))
}

/// Lists a project's runs newest first within the page budget, with no title for a run the
/// host's run sight gives the actor as its outcome only. Input: the context. Output: `runs`
/// and a `run:<n>` cursor naming the last run shown while more follow.
pub(super) fn run_list<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let input = RunList {
        project: cx.project().clone(),
        plan: cx.args.get::<&TestId>("plan").cloned(),
        batch: cx.args.get::<&str>("batch").map(str::to_owned),
        verdict: cx.args.get::<Verdict>("verdict"),
        commit: cx.args.get::<&str>("commit").map(str::to_owned),
        before: cursor(cx, "run", 1)?,
    };
    let listed = runs::list_as(cx.tx, cx.host, cx.actor, &input)?;
    let mut page = Page::new(cx.data_bytes_max);
    let taken = fill(&mut page, "runs", &listed.rows);
    assert!(
        listed.rows.is_empty() || taken > 0,
        "one run row always fits a page"
    );
    let next = if taken < listed.rows.len() {
        Some(listed.rows[taken - 1].id.number())
    } else {
        listed.cursor
    };
    Ok(done(page.into_value(), next.map(|n| format!("run:{n}"))))
}

/// Shows one case as the actor may see it, its latest outcome under the host's run sight:
/// its fields on the first page, then its steps from the cursor's `step:<n>` offset.
/// Input: the context. Output: the page and cursor.
pub(super) fn case_show<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let id = cx.args.need::<&TestId>("id");
    let view = tests_store::case_show_as(cx.tx, cx.host, cx.actor, id)?;
    let (shown, steps) = match &view.steps {
        StepsView::Hidden => ("hidden", &[][..]),
        StepsView::Headings(steps) => ("headings", &steps[..]),
        StepsView::Full(steps) => ("full", &steps[..]),
    };
    let at = cursor(cx, "step", 0)?;
    let mut page = Page::new(cx.data_bytes_max);
    let from = match at {
        None => {
            assert!(
                page.put("case", case_header(&view, shown)),
                "a case's fields fit a page"
            );
            0
        }
        Some(at) => continued(&mut page, id, "steps_at", at, steps.len())?,
    };
    let taken = fill(&mut page, "steps", &steps[from..]);
    assert!(at.is_none() || from == steps.len() || taken > 0);
    let next = (from + taken < steps.len()).then(|| format!("step:{}", from + taken));
    Ok(done(page.into_value(), next))
}

/// Shows one set: its fields on the first page, then its cases in order from the
/// cursor's `member:<n>` offset. Input: the context. Output: the page and cursor.
pub(super) fn set_show<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let id = cx.args.need::<&TestId>("id");
    let view = tests_store::set_show(cx.tx, id)?;
    let header = json!({
        "id": view.id.to_string(),
        "name": view.name,
        "version": view.version,
        "cases_total": view.cases.len(),
    });
    members(cx, id, "set", header, "cases", &view.cases)
}

/// Shows one plan: its fields on the first page, then its items in order from the
/// cursor's `member:<n>` offset. Input: the context. Output: the page and cursor.
pub(super) fn plan_show<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let id = cx.args.need::<&TestId>("id");
    let view = tests_store::plan_show(cx.tx, id)?;
    let header = json!({
        "id": view.id.to_string(),
        "name": view.name,
        "status": view.status,
        "release": view.release,
        "cycle": view.cycle,
        "version": view.version,
        "items_total": view.items.len(),
    });
    members(cx, id, "plan", header, "items", &view.items)
}

/// Shows one run: on the first page its fields, source, policy and integrity (a member
/// that cannot fit is named `<member>_cut`) and unlinked names, then its results from
/// the cursor's `result:<position>`; a protected case's message and artifact are withheld
/// from an actor its visibility narrows. A run the host's run sight gives the actor as its
/// outcome only shows `run` as that outcome and `sight` `outcome`, nothing else. Input: the
/// context. Output: the page and cursor.
pub(super) fn run_show<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let id = cx.args.need::<&TestId>("id");
    let after = cursor(cx, "result", 0)?;
    let input = RunShow {
        id: id.clone(),
        after,
    };
    let shown = match runs::show_as(cx.tx, cx.host, cx.actor, &input)? {
        Sighted::Full(shown) => *shown,
        Sighted::Outcome(outcome) => {
            assert_eq!(&outcome.id, id);
            return Ok(outcome_only(cx, &outcome));
        }
    };
    assert_eq!(&shown.run.id, id);
    let mut page = Page::new(cx.data_bytes_max);
    match after {
        None => {
            assert!(
                page.put("run", header(&shown.run)),
                "a run's fields fit a page"
            );
            put_or_cut(&mut page, "source", value(&shown.run.source));
            put_or_cut(&mut page, "policy", value(&shown.run.policy));
            put_or_cut(&mut page, "integrity", value(&shown.run.integrity));
            assert!(page.put("unlinked_total", json!(shown.unlinked_total)));
            fill(&mut page, "unlinked", &shown.unlinked);
        }
        Some(_) => assert!(page.put("id", json!(id.to_string()))),
    }
    let withheld = withheld_cases(cx, &shown.results)?;
    let rows: Vec<Value> = shown
        .results
        .iter()
        .map(|result| result_row(result, &withheld))
        .collect();
    let taken = fill(&mut page, "results", &rows);
    assert!(after.is_none() || shown.results.is_empty() || taken > 0);
    let next = if taken < shown.results.len() {
        let last = taken.checked_sub(1).map(|at| shown.results[at].position);
        Some(last.or(after).unwrap_or(0))
    } else {
        shown.cursor
    };
    Ok(done(
        page.into_value(),
        next.map(|at| format!("result:{at}")),
    ))
}

/// Lists each criterion of a ticket or release with its cases and their latest results,
/// leaving out each result from a run the host's run sight gives the actor as its outcome
/// only. The cursor `crit:<ticket number>:<position>` names the last criterion shown.
/// Input: the context. Output: the totals, `rows` and the cursor.
pub(super) fn coverage<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let project = cx.project();
    let ticket = cx.args.get::<&ShortId>("ticket");
    let release = cx.args.get::<&str>("release");
    let of = match (ticket, release) {
        (Some(ticket), None) => CoverageOf::Ticket(ticket.clone()),
        (None, Some(release)) => CoverageOf::Version(release.to_owned()),
        _ => return Err(one_of("ticket", "release")),
    };
    let after = criterion_cursor(cx)?;
    let query = CoverageQuery {
        project: project.clone(),
        of,
        after,
    };
    let found = coverage::coverage_as(cx.tx, cx.host, cx.actor, &query)?;
    let mut page = Page::new(cx.data_bytes_max);
    assert!(page.put("criteria_total", json!(found.criteria_total)));
    assert!(page.put("uncovered_total", json!(found.uncovered_total)));
    let taken = fill(&mut page, "rows", &found.rows);
    assert!(
        found.rows.is_empty() || taken > 0,
        "one coverage row always fits a page"
    );
    let last = if taken < found.rows.len() {
        Some(found.rows[taken - 1].criterion.clone())
    } else {
        found.cursor
    };
    let next = last.map(|at| format!("crit:{}:{}", at.ticket.number(), at.position));
    Ok(done(page.into_value(), next))
}

/// Creates a case. Input: the context. Output: the new case's id and version.
pub(super) fn case_create<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let case = NewCase {
        title: cx.args.need::<&str>("title").to_owned(),
        kind: cx.args.need::<CaseKind>("kind"),
        stable_name: cx.args.get::<&str>("stable_name").map(str::to_owned),
        steps: cx
            .args
            .get::<&Value>("steps")
            .map(shaped)
            .unwrap_or_default(),
        protected: cx.args.get::<bool>("protected").unwrap_or(false),
        status: cx
            .args
            .get::<CaseStatus>("status")
            .unwrap_or(CaseStatus::Draft),
    };
    let created = tests_store::create_case(cx.tx, cx.actor, cx.project(), &case, cx.at_ms)?;
    assert_eq!(created.version, 1);
    Ok(written("case", &created.short_id, created.version))
}

/// Changes a case at the version read. Input: the context. Output: its id and version.
pub(super) fn case_edit<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let named = cx.args.get::<&str>("stable_name");
    let clear = cx.args.get::<bool>("clear_stable_name").unwrap_or(false);
    if clear && named.is_some() {
        return Err(Failure::new(
            codes::FIELD_INVALID,
            "give stable_name or clear_stable_name, not both",
        ));
    }
    let change = CaseChange {
        title: cx.args.get::<&str>("title").map(str::to_owned),
        kind: cx.args.get::<CaseKind>("kind"),
        stable_name: if clear {
            Some(None)
        } else {
            named.map(|name| Some(name.to_owned()))
        },
        steps: cx.args.get::<&Value>("steps").map(shaped),
        protected: cx.args.get::<bool>("protected"),
        status: cx.args.get::<CaseStatus>("status"),
    };
    let (id, version) = target(cx);
    let edited = tests_store::edit_case(cx.tx, cx.actor, id, version, &change, cx.at_ms)?;
    assert!(edited.version > version);
    Ok(written("case", &edited.short_id, edited.version))
}

/// Links a case to one criterion. Input: the context. Output: the case's id and version.
pub(super) fn case_link<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let (id, version) = target(cx);
    let criterion = criterion_of(cx);
    let linked = tests_store::link_case(cx.tx, cx.actor, id, version, &criterion, cx.at_ms)?;
    assert!(linked.version > version);
    Ok(written("case", &linked.short_id, linked.version))
}

/// Removes a case's link to one criterion. Input: the context. Output: the case's id and
/// version.
pub(super) fn case_unlink<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let (id, version) = target(cx);
    let criterion = criterion_of(cx);
    let unlinked = tests_store::unlink_case(cx.tx, cx.actor, id, version, &criterion, cx.at_ms)?;
    assert!(unlinked.version > version);
    Ok(written("case", &unlinked.short_id, unlinked.version))
}

/// Creates a set, or renames one at the version read. Input: the context. Output: the
/// set's id and version.
pub(super) fn set_upsert<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let existing = existing(cx)?;
    let name = cx.args.need::<&str>("name");
    let set = tests_store::upsert_set(cx.tx, cx.actor, cx.project(), existing, name, cx.at_ms)?;
    assert!(existing.is_none_or(|(_, version)| set.version > version));
    Ok(written("set", &set.short_id, set.version))
}

/// Adds a case to the end of a set. Input: the context. Output: the set's id and version.
pub(super) fn set_add<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let (id, version) = target(cx);
    let case = cx.args.need::<&TestId>("case");
    let set = tests_store::set_add(cx.tx, cx.actor, id, version, case, cx.at_ms)?;
    assert!(set.version > version);
    Ok(written("set", &set.short_id, set.version))
}

/// Removes a case from a set. Input: the context. Output: the set's id and version.
pub(super) fn set_remove<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let (id, version) = target(cx);
    let case = cx.args.need::<&TestId>("case");
    let set = tests_store::set_remove(cx.tx, cx.actor, id, version, case, cx.at_ms)?;
    assert!(set.version > version);
    Ok(written("set", &set.short_id, set.version))
}

/// Creates a plan, or sets one's fields at the version read. Input: the context. Output:
/// the plan's id and version.
pub(super) fn plan_upsert<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let existing = existing(cx)?;
    let fields = PlanFields {
        name: cx.args.need::<&str>("name").to_owned(),
        release: cx.args.get::<&str>("release").map(str::to_owned),
        cycle: cx
            .args
            .get::<i64>("cycle")
            .map(|cycle| bounded(cycle, "cycle")),
        status: cx
            .args
            .get::<PlanStatus>("status")
            .unwrap_or(PlanStatus::Draft),
    };
    let plan =
        tests_store::upsert_plan(cx.tx, cx.actor, cx.project(), existing, &fields, cx.at_ms)?;
    assert!(existing.is_none_or(|(_, version)| plan.version > version));
    Ok(written("plan", &plan.short_id, plan.version))
}

/// Adds one case or one set to the end of a plan. Input: the context. Output: the plan's
/// id and version.
pub(super) fn plan_add<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let (id, version) = target(cx);
    let item = item_of(cx)?;
    let plan = tests_store::plan_add(cx.tx, cx.actor, id, version, &item, cx.at_ms)?;
    assert!(plan.version > version);
    Ok(written("plan", &plan.short_id, plan.version))
}

/// Removes one case or one set from a plan. Input: the context. Output: the plan's id and
/// version.
pub(super) fn plan_remove<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let (id, version) = target(cx);
    let item = item_of(cx)?;
    let plan = tests_store::plan_remove(cx.tx, cx.actor, id, version, &item, cx.at_ms)?;
    assert!(plan.version > version);
    Ok(written("plan", &plan.short_id, plan.version))
}

/// Starts a run with its source and policy bound. Input: the context. Output: the run's
/// id, version, state, verdict and totals.
pub(super) fn run_start<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let input = RunStart {
        project: cx.project().clone(),
        title: cx.args.need::<&str>("title").to_owned(),
        plan: cx.args.get::<&TestId>("plan").cloned(),
        batch: cx.args.get::<&str>("batch").map(str::to_owned),
        source: shaped(cx.args.need::<&Value>("source")),
        policy: shaped(cx.args.need::<&Value>("policy")),
    };
    let run = runs::start(cx.tx, cx.actor, &input, cx.at_ms)?;
    assert_eq!(run.id.key(), cx.project());
    Ok(done(json!({"run": run_ack(&run)}), None))
}

/// Appends one batch of results to a running run. Input: the context. Output: the run's
/// id, version, state, verdict and totals.
pub(super) fn run_record<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let (id, version) = target(cx);
    let input = RunRecord {
        id: id.clone(),
        version,
        results: shaped(cx.args.need::<&Value>("results")),
    };
    let run = runs::record(cx.tx, cx.actor, &input, cx.at_ms)?;
    assert!(run.version > version);
    Ok(done(json!({"run": run_ack(&run)}), None))
}

/// Finishes a running run and re-evaluates the tickets whose test checks it reported, in
/// the same transaction. Input: the context. Output: the run and the tickets it touched,
/// as many as fit, with their total.
pub(super) fn run_finish<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let (id, version) = target(cx);
    let input = RunFinish {
        id: id.clone(),
        version,
        end: cx.args.need::<RunEnd>("end"),
        exit_code: cx
            .args
            .get::<i64>("exit_code")
            .map(|code| bounded(code, "exit_code")),
        reason: cx.args.get::<&str>("reason").map(str::to_owned),
        integrity: cx.args.get::<&Value>("integrity").map(shaped),
    };
    let ended = coverage::finish(cx.tx, cx.host, cx.actor, &input, cx.at_ms)?;
    assert!(ended.run.version > version);
    Ok(finished(cx, &ended))
}

/// Interrupts a running run and re-evaluates the tickets whose test checks it reported,
/// in the same transaction. Input: the context. Output: the run and the tickets it
/// touched, as many as fit, with their total.
pub(super) fn run_cancel<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Outcome, Failure> {
    let (id, version) = target(cx);
    let input = RunCancel {
        id: id.clone(),
        version,
        reason: cx.args.need::<StopReason>("reason"),
    };
    let cancelled = coverage::cancel(cx.tx, cx.host, cx.actor, &input, cx.at_ms)?;
    assert!(cancelled.run.version > version);
    Ok(finished(cx, &cancelled))
}

fn done(data: Value, cursor: Option<String>) -> Outcome {
    assert!(data.is_object());
    Outcome {
        data,
        next: Vec::new(),
        cursor,
    }
}

fn value<T: Serialize>(view: &T) -> Value {
    match serde_json::to_value(view) {
        Ok(value) => value,
        Err(error) => panic!("a test view serializes: {error}"),
    }
}

fn shaped<'v, T: Deserialize<'v>>(value: &'v Value) -> T {
    match T::deserialize(value) {
        Ok(read) => read,
        Err(error) => panic!("request parsing checked this shape: {error}"),
    }
}

fn bounded<T: TryFrom<i64>>(number: i64, field: &str) -> T {
    match T::try_from(number) {
        Ok(read) => read,
        Err(_) => panic!("the catalog bounds {field}, so {number} fits"),
    }
}

fn written(object: &str, id: &TestId, version: i64) -> Outcome {
    assert!(version >= 1);
    done(
        json!({object: {"id": id.to_string(), "version": version}}),
        None,
    )
}

fn finished<H: Host>(cx: &Cx<'_, '_, H>, ended: &Finished) -> Outcome {
    assert!(ended.run.finished_ms.is_some());
    let mut page = Page::new(cx.data_bytes_max);
    assert!(page.put("run", run_ack(&ended.run)));
    assert!(page.put("tickets_total", json!(ended.tickets.len())));
    fill(&mut page, "tickets", &ended.tickets);
    done(page.into_value(), None)
}

fn withheld_cases<H: Host>(
    cx: &Cx<'_, '_, H>,
    results: &[ResultView],
) -> Result<BTreeSet<TestId>, Failure> {
    let mut withheld = BTreeSet::new();
    if sees_protected(cx.actor) {
        return Ok(withheld);
    }
    let cases: BTreeSet<&TestId> = results
        .iter()
        .filter_map(|result| result.case.as_ref())
        .collect();
    for case in cases {
        let found = tests_store::case(cx.tx, case)?;
        if !found.protected {
            continue;
        }
        let visibility = tests_store::protected_visibility(cx.tx, found.project)?;
        if visibility != CaseProtectedVisibility::Full {
            withheld.insert(case.clone());
        }
    }
    assert!(withheld.len() <= results.len());
    Ok(withheld)
}

fn result_row(result: &ResultView, withheld: &BTreeSet<TestId>) -> Value {
    let mut row = value(result);
    if result
        .case
        .as_ref()
        .is_some_and(|case| withheld.contains(case))
    {
        let fields = row.as_object_mut().expect("a result view is an object");
        assert!(fields.insert("message".to_owned(), Value::Null).is_some());
        assert!(fields.insert("artifact".to_owned(), Value::Null).is_some());
        fields.insert("withheld".to_owned(), json!(true));
    }
    row
}

fn run_ack(run: &RunView) -> Value {
    json!({
        "id": run.id.to_string(),
        "version": run.version,
        "state": run.state,
        "verdict": run.verdict,
        "totals": run.totals,
    })
}

/// The page of a run the actor reads as its outcome only: `run` is the outcome and `sight`
/// is `outcome`, with no source, policy, integrity, unlinked name, result or cursor.
/// Inputs: the context and the outcome. Output: the page.
fn outcome_only<H: Host>(cx: &Cx<'_, '_, H>, outcome: &RunOutcome) -> Outcome {
    let mut page = Page::new(cx.data_bytes_max);
    assert!(
        page.put("run", value(outcome)),
        "a run's outcome fits a page"
    );
    assert!(page.put("sight", json!("outcome")));
    done(page.into_value(), None)
}

fn header(run: &RunView) -> Value {
    let mut header = value(run);
    let fields = header.as_object_mut().expect("a run view is an object");
    for bound in ["source", "policy", "integrity"] {
        assert!(fields.remove(bound).is_some(), "a run view holds {bound}");
    }
    header
}

fn case_header(view: &CaseView, shown: &str) -> Value {
    let mut header = value(view);
    let fields = header.as_object_mut().expect("a case view is an object");
    assert!(
        fields.remove("steps").is_some(),
        "a case view holds its steps"
    );
    fields.insert("shown".to_owned(), json!(shown));
    let total = fields
        .get_mut("detail")
        .and_then(|detail| detail.get_mut("sets"))
        .and_then(Value::as_array_mut)
        .map(|sets| {
            let total = sets.len();
            sets.truncate(LIST_ROWS_MAX);
            total
        });
    if let Some(total) = total {
        fields.insert("sets_total".to_owned(), json!(total));
    }
    header
}

fn put_or_cut(page: &mut Page, name: &str, member: Value) {
    if !page.put(name, member) {
        assert!(page.put(&format!("{name}_cut"), json!(true)));
    }
}

fn fill<T: Serialize>(page: &mut Page, name: &str, rows: &[T]) -> usize {
    assert!(page.put(name, json!([])), "an empty list fits a page");
    let taken = rows
        .iter()
        .take_while(|row| page.push(name, value(row)))
        .count();
    assert!(taken <= rows.len());
    taken
}

fn members<H: Host, T: Serialize>(
    cx: &Cx<'_, '_, H>,
    id: &TestId,
    object: &str,
    header: Value,
    name: &str,
    rows: &[T],
) -> Result<Outcome, Failure> {
    let at = cursor(cx, "member", 0)?;
    let mut page = Page::new(cx.data_bytes_max);
    let from = match at {
        None => {
            assert!(page.put(object, header), "a {object}'s fields fit a page");
            0
        }
        Some(at) => continued(&mut page, id, &format!("{name}_at"), at, rows.len())?,
    };
    let taken = fill(&mut page, name, &rows[from..]);
    assert!(at.is_none() || from == rows.len() || taken > 0);
    let next = (from + taken < rows.len()).then(|| format!("member:{}", from + taken));
    Ok(done(page.into_value(), next))
}

fn continued(
    page: &mut Page,
    id: &TestId,
    offset: &str,
    at: u32,
    len: usize,
) -> Result<usize, Failure> {
    let at = usize::try_from(at).expect("a u32 offset fits usize");
    if at > len {
        return Err(cursor_invalid());
    }
    assert!(page.put("id", json!(id.to_string())));
    assert!(page.put(offset, json!(at)));
    Ok(at)
}

fn cursor<H: Host>(cx: &Cx<'_, '_, H>, prefix: &str, min: u32) -> Result<Option<u32>, Failure> {
    let Some(text) = cx.args.get::<&str>("cursor") else {
        return Ok(None);
    };
    let number = text
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix(':'))
        .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|digits| digits.parse::<u32>().ok())
        .filter(|number| *number >= min);
    number.map(Some).ok_or_else(cursor_invalid)
}

fn criterion_cursor<H: Host>(cx: &Cx<'_, '_, H>) -> Result<Option<CriterionRef>, Failure> {
    let Some(text) = cx.args.get::<&str>("cursor") else {
        return Ok(None);
    };
    let parts = text
        .strip_prefix("crit:")
        .and_then(|rest| rest.split_once(':'))
        .and_then(|(number, position)| {
            Some((number.parse::<u32>().ok()?, position.parse::<u32>().ok()?))
        })
        .filter(|(number, position)| *number >= 1 && *position >= 1);
    let (number, position) = parts.ok_or_else(cursor_invalid)?;
    let ticket = ShortId::new(cx.project().clone(), number).map_err(|_| cursor_invalid())?;
    Ok(Some(CriterionRef { ticket, position }))
}

fn cursor_invalid() -> Failure {
    Failure::new(
        codes::CURSOR_INVALID,
        "the cursor is not one this operation gave; start again without it",
    )
}

fn one_of(first: &str, second: &str) -> Failure {
    Failure::new(
        codes::FIELD_INVALID,
        format!("give exactly one of {first} and {second}"),
    )
}

fn target<'a, H: Host>(cx: &'a Cx<'_, '_, H>) -> (&'a TestId, i64) {
    let version = cx.args.need::<i64>("version");
    assert!(version >= 1, "the catalog bounds versions from 1");
    (cx.args.need::<&TestId>("id"), version)
}

fn existing<'a, H: Host>(cx: &'a Cx<'_, '_, H>) -> Result<Option<(&'a TestId, i64)>, Failure> {
    match (cx.args.get::<&TestId>("id"), cx.args.get::<i64>("version")) {
        (Some(id), Some(version)) => Ok(Some((id, version))),
        (None, None) => Ok(None),
        _ => Err(Failure::new(
            codes::FIELD_INVALID,
            "id and version go together: both to change one, neither to create",
        )),
    }
}

fn criterion_of<H: Host>(cx: &Cx<'_, '_, H>) -> CriterionRef {
    let position = cx.args.need::<i64>("position");
    CriterionRef {
        ticket: cx.args.need::<&ShortId>("ticket").clone(),
        position: bounded(position, "position"),
    }
}

fn item_of<H: Host>(cx: &Cx<'_, '_, H>) -> Result<PlanItemRef, Failure> {
    match (
        cx.args.get::<&TestId>("case"),
        cx.args.get::<&TestId>("set"),
    ) {
        (Some(case), None) => Ok(PlanItemRef::Case(case.clone())),
        (None, Some(set)) => Ok(PlanItemRef::Set(set.clone())),
        _ => Err(one_of("case", "set")),
    }
}
