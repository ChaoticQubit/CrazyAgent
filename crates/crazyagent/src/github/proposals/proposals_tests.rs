use super::*;
use crate::credentials::test_keychain::TestKeychain;
use crate::credentials::{Keychain, SecretString, Store, reference};
use crate::github::test_server::{Answer, Server};
use crate::github::{
    Endpoints, GitHub, GitHubPublishJob, GitHubPublishPlan, GitHubPublishStep, GitHubRuleOutcome,
    GitHubRuleState,
};
use crate::protocol::SessionId;
use crate::questions::{Answer as Picked, Question, QuestionKind, Settlement};
use rusqlite::TransactionBehavior;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

const PROJECT: &str = "alpha";
const OTHER: &str = "beta";
const REPO: &str = "octo-cat/tool";
const TOKEN: &str = "gho_proposalstandin0123456789";
const WHY: &str = "Reviews keep merging with threads still open.";
const REFUSAL: &str = "Resource not accessible by integration";

/// One engine root with two set-up projects, alpha and beta, and the engine database with
/// the team, setup and questions tables, as the proposal rows use.
struct Case {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    folder: PathBuf,
    other: PathBuf,
    db: Connection,
    project: ProjectId,
}

impl Case {
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("ca-proposals-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("engine");
        let folder = base.join(PROJECT);
        let other = base.join(OTHER);
        for path in [&root, &folder, &other] {
            std::fs::create_dir(path).unwrap();
        }
        let db = crate::questions::open(&root).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        crate::team::initialize(&db).unwrap();
        crate::ide::workspace::tests::add_project(&db, PROJECT, &folder);
        crate::ide::workspace::tests::add_project(&db, OTHER, &other);
        Self {
            _tmp: tmp,
            root,
            folder,
            other,
            db,
            project: ProjectId::new(PROJECT).unwrap(),
        }
    }

    fn publish(&self, folder: &Path, step: GitHubPublishStep) {
        super::super::state::save_job(&self.root, &job(folder, step)).unwrap();
    }

    fn done(&self, repo: &str) {
        self.publish(
            &self.folder,
            GitHubPublishStep::Done {
                full_name: repo.into(),
                outcomes: Vec::new(),
            },
        );
    }

    fn propose(&self, asker: &Asker, rule: GitHubRule, on: bool) -> Result<u64, String> {
        request(&self.db, &self.project, asker, rule, on, WHY)
    }

    fn asked(&self, asker: &Asker, rule: GitHubRule, on: bool) -> u64 {
        let asked = self.propose(asker, rule, on);
        assert!(asked.is_ok(), "the proposal raised no question: {asked:?}");
        asked.unwrap()
    }

    fn questions(&self) -> Vec<Question> {
        crate::questions::page(&self.db, &self.project, None)
            .unwrap()
            .items
    }

    fn statuses(&self) -> Vec<String> {
        let Ok(mut statement) = self
            .db
            .prepare("SELECT status FROM github_rule_requests ORDER BY id")
        else {
            return Vec::new();
        };
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn told(&self) -> Vec<String> {
        let mut statement = self
            .db
            .prepare(
                "SELECT text FROM team_messages WHERE project=?1 AND author='engine' ORDER BY id",
            )
            .unwrap();
        statement
            .query_map([PROJECT], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn answer(&self, question: u64, picks: Vec<u8>) -> Settlement {
        let picked = Picked {
            picks,
            comment: None,
        };
        crate::questions::answer(&self.db, question, 0, &picked).unwrap()
    }
}

fn job(project: &Path, step: GitHubPublishStep) -> GitHubPublishJob {
    GitHubPublishJob {
        project: project.to_path_buf(),
        plan: GitHubPublishPlan {
            owner: "octo-cat".into(),
            name: "tool".into(),
            description: String::new(),
            private: false,
            docs: false,
        },
        lines: Vec::new(),
        written: Vec::new(),
        step,
        stopped: None,
        made: Some(REPO.into()),
        rules: GitHubRule::defaults(),
    }
}

fn page(rule: GitHubRule, on: bool, state: GitHubRuleState) -> GitHubRepoRules {
    GitHubRepoRules {
        full_name: REPO.into(),
        private: false,
        checks: Vec::new(),
        rules: vec![GitHubRuleOutcome { rule, on, state }],
    }
}

fn plain_for_the_agent(text: &str) {
    for secret in [REPO, "octo-cat", TOKEN, REFUSAL, "403"] {
        assert!(!text.contains(secret), "{secret} reached the agent: {text}");
    }
}

/// DEC-182 S7: an agent's proposal raises one Single owner question from the engine, asked
/// by `engine:github-rule:<id>`, with Approve and Decline, naming the rule, on or off and
/// the repository the asker's own project was published to. The agent's reason shows only
/// as its own quoted words, cut to 300 bytes; turning a protection on recommends Approve
/// and turning one off recommends Decline. Both clients label the asker.
#[test]
fn a_proposal_raises_one_owner_question_naming_the_rule_and_its_repository_with_the_agents_words_quoted()
 {
    let case = Case::new();
    case.done(REPO);
    let question = case.asked(&Asker::Agent(4), GitHubRule::ConversationResolution, true);
    let asked = case.questions();
    assert_eq!(asked.len(), 1);
    let on = &asked[0];
    assert_eq!(on.id, question);
    assert!(request_id(&on.asker).is_some(), "{}", on.asker);
    assert!(on.asker.starts_with(ASKER_PREFIX), "{}", on.asker);
    assert_eq!(on.kind, QuestionKind::Single);
    assert_eq!(on.options, [APPROVE, DECLINE]);
    assert_eq!(on.recommended.as_deref(), Some(APPROVE));
    for word in ["Agent 4", "conversation_resolution", " on ", REPO] {
        assert!(on.question.contains(word), "{word}: {}", on.question);
    }
    let context = on.context.clone().unwrap();
    assert!(
        context.contains(&serde_json::to_string(WHY).unwrap()),
        "{context}"
    );
    assert_eq!(case.statuses(), ["pending"]);

    let long = format!("{}{}", "x".repeat(WHY_BYTES_MAX), "y".repeat(50));
    let off = request(
        &case.db,
        &case.project,
        &Asker::Master(7),
        GitHubRule::NoForcePush,
        false,
        &long,
    );
    assert!(off.is_ok(), "{off:?}");
    let asked = case.questions();
    let off = asked.iter().find(|item| item.id == off.unwrap()).unwrap();
    assert_eq!(off.recommended.as_deref(), Some(DECLINE));
    assert!(off.question.contains("no_force_push") && off.question.contains(" off "));
    let context = off.context.clone().unwrap();
    let cut = serde_json::to_string(&"x".repeat(WHY_BYTES_MAX)).unwrap();
    assert!(
        context.contains(&cut),
        "the reason is cut at 300 bytes: {context}"
    );
    let asker = &off.asker;
    let number = asker.strip_prefix(ASKER_PREFIX).unwrap();
    assert_eq!(
        crate::questions::asker_label(asker),
        format!("GitHub rule proposal {number}")
    );
}

/// DEC-182 S7: a proposal is refused before any question or proposal row exists when the
/// asker's own project has no publish at Done, even while another project's is done, for a
/// rule that is a file in the project, for a conversation master or a terminal, and for a
/// reason that is empty or holds hidden characters.
#[test]
fn a_proposal_is_refused_before_any_question_without_its_projects_own_published_repository() {
    let case = Case::new();
    let refused = |asker: &Asker, rule: GitHubRule, why: &str| {
        request(&case.db, &case.project, asker, rule, true, why)
    };
    let agent = Asker::Agent(4);
    assert_eq!(
        refused(&agent, GitHubRule::WikiOff, WHY),
        Err("github_rules_not_published".to_owned())
    );
    case.publish(
        &case.folder,
        GitHubPublishStep::Pushed {
            full_name: REPO.into(),
            private: false,
            workflows: true,
        },
    );
    assert_eq!(
        refused(&agent, GitHubRule::WikiOff, WHY),
        Err("github_rules_not_published".to_owned())
    );
    case.publish(
        &case.other,
        GitHubPublishStep::Done {
            full_name: "octo-cat/beta".into(),
            outcomes: Vec::new(),
        },
    );
    assert_eq!(
        refused(&agent, GitHubRule::WikiOff, WHY),
        Err("github_rules_not_published".to_owned()),
        "another project's repository is never this project's"
    );
    case.done(REPO);
    for rule in [
        GitHubRule::DependabotVersionUpdates,
        GitHubRule::SecurityPolicy,
    ] {
        assert_eq!(
            refused(&agent, rule, WHY),
            Err("github_rule_invalid".to_owned())
        );
    }
    let session = Asker::Session(SessionId::new("session-1").unwrap());
    for asker in [session, Asker::Terminal(3), Asker::Agent(0)] {
        assert_eq!(
            refused(&asker, GitHubRule::WikiOff, WHY),
            Err("github_rule_asker_invalid".to_owned())
        );
    }
    for why in ["   ", "open\u{0}threads", "two\nlines", "bell\u{7}"] {
        assert_eq!(
            refused(&agent, GitHubRule::WikiOff, why),
            Err("github_rule_why_invalid".to_owned()),
            "{why:?}"
        );
    }
    assert!(case.questions().is_empty());
    assert!(case.statuses().is_empty());
}

/// DEC-182 S7: one agent has at most one open proposal per rule, whatever value it asks,
/// and a project at most four open at once; a decided one makes room.
#[test]
fn one_open_proposal_per_agent_per_rule_and_four_per_project() {
    let case = Case::new();
    case.done(REPO);
    let first = case.asked(&Asker::Agent(4), GitHubRule::WikiOff, false);
    assert_eq!(
        case.propose(&Asker::Agent(4), GitHubRule::WikiOff, true),
        Err("github_rule_request_open".to_owned())
    );
    case.asked(&Asker::Agent(5), GitHubRule::WikiOff, false);
    case.asked(&Asker::Agent(4), GitHubRule::LinearHistory, true);
    case.asked(&Asker::Master(7), GitHubRule::NoDeletion, true);
    assert_eq!(
        case.propose(&Asker::Agent(6), GitHubRule::TagProtection, true),
        Err("github_rule_requests_full".to_owned())
    );
    assert_eq!(case.questions().len(), OPEN_PER_PROJECT_MAX as usize);
    assert_eq!(
        crate::questions::dismiss(&case.db, first, 0).unwrap(),
        Settlement::Settled
    );
    case.asked(&Asker::Agent(4), GitHubRule::WikiOff, true);
    assert_eq!(
        case.propose(&Asker::Agent(6), GitHubRule::TagProtection, true),
        Err("github_rule_requests_full".to_owned())
    );
}

/// DEC-182 S7: Approve only marks the proposal approved, sends nothing and tells nothing
/// yet; the apply that runs after the answer sends exactly the one change it recorded, on
/// the asker's project folder, once, and the agent is told only "approved and applied".
#[test]
fn approve_sends_nothing_until_the_apply_which_sends_the_one_recorded_change_once() {
    let case = Case::new();
    case.done(REPO);
    let question = case.asked(&Asker::Agent(4), GitHubRule::PullRequest, true);
    assert_eq!(case.answer(question, vec![0]), Settlement::Settled);
    assert_eq!(case.statuses(), ["approved"]);
    assert!(case.told().is_empty(), "{:?}", case.told());
    let mut calls = Vec::new();
    let mut set = |folder: &Path, rule: GitHubRule, on: bool| {
        calls.push((folder.to_path_buf(), rule, on));
        Ok(page(rule, on, GitHubRuleState::Applied))
    };
    assert_eq!(apply_approved(&case.db, &mut set).unwrap(), 1);
    assert_eq!(apply_approved(&case.db, &mut set).unwrap(), 0);
    assert_eq!(
        calls,
        [(case.folder.clone(), GitHubRule::PullRequest, true)]
    );
    assert_eq!(case.statuses(), ["applied"]);
    let told = case.told();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("pull_request on"), "{}", told[0]);
    assert!(told[0].ends_with(": approved and applied."), "{}", told[0]);
    plain_for_the_agent(&told[0]);
}

/// DEC-182 S7: GitHub refusing the approved change, or its page not showing the rule as
/// wanted and applied, ends the proposal refused; the agent is told only "refused".
#[test]
fn a_change_github_refuses_or_does_not_show_applied_is_told_only_as_refused() {
    let case = Case::new();
    case.done(REPO);
    let refused = case.asked(&Asker::Agent(4), GitHubRule::SecretScanning, true);
    let unshown = case.asked(&Asker::Agent(5), GitHubRule::CodeqlDefaultSetup, true);
    assert_eq!(case.answer(refused, vec![0]), Settlement::Settled);
    assert_eq!(case.answer(unshown, vec![0]), Settlement::Settled);
    let mut set = |_: &Path, rule: GitHubRule, on: bool| {
        Ok(match rule {
            GitHubRule::SecretScanning => page(
                rule,
                on,
                GitHubRuleState::Refused {
                    status: 403,
                    message: REFUSAL.into(),
                },
            ),
            _ => page(rule, !on, GitHubRuleState::Applied),
        })
    };
    assert_eq!(apply_approved(&case.db, &mut set).unwrap(), 2);
    assert_eq!(case.statuses(), ["refused", "refused"]);
    let told = case.told();
    assert_eq!(told.len(), 2, "{told:?}");
    for text in &told {
        assert!(text.ends_with(": refused."), "{text}");
        plain_for_the_agent(text);
    }
}

/// DEC-182 S7: the apply runs with the owner's own GitHub sign-in on the repository the
/// proposal recorded, and whatever GitHub answers, its words and the token never reach
/// the agent.
#[test]
fn the_apply_uses_the_owners_sign_in_on_the_recorded_repository_and_tells_the_agent_nothing_of_it()
{
    let case = Case::new();
    case.done(REPO);
    let mut bytes = [0_u8; 6];
    getrandom::fill(&mut bytes).unwrap();
    let suffix: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let namespace = format!("dev.crazyagent.test-{suffix}");
    let mut keychain = TestKeychain::temporary();
    keychain.serve(&namespace);
    let store = Arc::new(Keychain::new(&namespace).unwrap());
    let server = Server::start(|seen, _| {
        if seen.method == "GET" && seen.path == "/user" {
            let mut answer = Answer::json(200, json!({"login": "octo-cat"}));
            answer.headers.push((
                "X-OAuth-Scopes".into(),
                "repo, read:user, read:org, workflow".into(),
            ));
            return answer;
        }
        Answer::json(403, json!({"message": REFUSAL}))
    });
    let account = GitHub::new(
        &case.root,
        namespace.clone(),
        Arc::clone(&store) as Arc<dyn Store>,
        Endpoints::loopback(server.port),
    );
    account
        .use_token(SecretString::new(TOKEN.as_bytes().to_vec()).unwrap())
        .unwrap();
    let signed_in = server.seen().len();
    let question = case.asked(&Asker::Agent(4), GitHubRule::NoForcePush, true);
    assert_eq!(server.seen().len(), signed_in, "a proposal sends nothing");
    assert_eq!(case.answer(question, vec![0]), Settlement::Settled);
    assert_eq!(server.seen().len(), signed_in, "an answer sends nothing");
    let mut set =
        |folder: &Path, rule: GitHubRule, on: bool| account.set_repo_rule(folder, rule, on);
    assert_eq!(apply_approved(&case.db, &mut set).unwrap(), 1);
    let sent: Vec<_> = server.seen().into_iter().skip(signed_in).collect();
    assert!(!sent.is_empty(), "the apply asked GitHub");
    for seen in &sent {
        assert!(
            seen.path.starts_with("/repos/octo-cat/tool"),
            "{}",
            seen.path
        );
        let authorization = seen.header("authorization").unwrap_or_default();
        assert!(authorization.contains(TOKEN), "{authorization}");
    }
    assert_eq!(case.statuses(), ["refused"]);
    let told = case.told();
    assert_eq!(told.len(), 1, "{told:?}");
    plain_for_the_agent(&told[0]);
    let item = reference(&format!("{namespace}.github"), "token").unwrap();
    let _ = store.delete(&item);
}

/// DEC-182 S7: Decline, a comment alone and a dismissal each decline the proposal; nothing
/// is sent, and the agent is told only "declined", never the generic answer message.
#[test]
fn decline_a_comment_alone_and_dismiss_send_nothing_and_tell_declined() {
    let case = Case::new();
    case.done(REPO);
    let declined = case.asked(&Asker::Agent(4), GitHubRule::WikiOff, false);
    let commented = case.asked(&Asker::Agent(5), GitHubRule::NoDeletion, false);
    let dismissed = case.asked(&Asker::Master(7), GitHubRule::LinearHistory, false);
    assert_eq!(case.answer(declined, vec![1]), Settlement::Settled);
    let comment = Picked {
        picks: Vec::new(),
        comment: Some("Not yet.".into()),
    };
    assert_eq!(
        crate::questions::answer(&case.db, commented, 0, &comment).unwrap(),
        Settlement::Settled
    );
    assert_eq!(
        crate::questions::dismiss(&case.db, dismissed, 0).unwrap(),
        Settlement::Settled
    );
    assert_eq!(case.statuses(), ["declined", "declined", "declined"]);
    let mut calls = 0;
    let mut set = |_: &Path, rule: GitHubRule, on: bool| {
        calls += 1;
        Ok(page(rule, on, GitHubRuleState::Applied))
    };
    assert_eq!(apply_approved(&case.db, &mut set).unwrap(), 0);
    assert_eq!(calls, 0);
    let told = case.told();
    assert_eq!(told.len(), 3, "{told:?}");
    for text in &told {
        assert!(text.ends_with(": declined."), "{text}");
        assert!(!text.contains("The owner"), "{text}");
        plain_for_the_agent(text);
    }
}

/// DEC-182 S7: Approve applies only the repository recorded at ask time. When the project's
/// publish record names another repository, or no longer is at Done, the approved
/// proposal is refused and nothing is sent.
#[test]
fn a_repository_changed_since_the_ask_is_refused_at_apply_with_nothing_sent() {
    let case = Case::new();
    case.done(REPO);
    let renamed = case.asked(&Asker::Agent(4), GitHubRule::LinearHistory, true);
    assert_eq!(case.answer(renamed, vec![0]), Settlement::Settled);
    case.done("octo-cat/other");
    let mut calls = 0;
    let mut set = |_: &Path, rule: GitHubRule, on: bool| {
        calls += 1;
        Ok(page(rule, on, GitHubRuleState::Applied))
    };
    assert_eq!(apply_approved(&case.db, &mut set).unwrap(), 1);
    case.done(REPO);
    let undone = case.asked(&Asker::Agent(5), GitHubRule::NoDeletion, true);
    assert_eq!(case.answer(undone, vec![0]), Settlement::Settled);
    super::super::state::remove_job(&case.root, &case.folder).unwrap();
    assert_eq!(apply_approved(&case.db, &mut set).unwrap(), 1);
    assert_eq!(calls, 0);
    assert_eq!(case.statuses(), ["refused", "refused"]);
}

/// DEC-182 S7: only the owner decides a proposal. A master can neither relay an answer it
/// says the owner typed nor record one from its chat, so the proposal stays waiting and
/// nothing is applied.
#[test]
fn a_master_cannot_relay_or_record_an_answer_to_a_proposal() {
    let case = Case::new();
    case.done(REPO);
    let question = case.asked(&Asker::Agent(4), GitHubRule::PullRequest, false);
    let key = crate::team::now_ms() + 60_000;
    let relayed =
        crate::questions::relay(&case.db, &case.project, 7, question, 0, APPROVE, Some(key));
    assert_eq!(relayed.unwrap_err().0, "questions_not_found");
    let recorded = crate::questions::record_from_chat(&case.db, &case.project, 7, question, 0, 1);
    assert_eq!(recorded.unwrap_err().0, "questions_not_found");
    assert_eq!(case.statuses(), ["pending"]);
    let mut calls = 0;
    let mut set = |_: &Path, rule: GitHubRule, on: bool| {
        calls += 1;
        Ok(page(rule, on, GitHubRuleState::Applied))
    };
    assert_eq!(apply_approved(&case.db, &mut set).unwrap(), 0);
    assert_eq!(calls, 0);
}

/// DEC-182 S7: a proposal an engine stop left approved, or claimed and being applied, ends
/// refused at the next start and its agent is told; the start sends nothing and no later
/// apply sends it either.
#[test]
fn a_start_ends_an_approved_or_applying_proposal_refused_and_tells_it_without_sending() {
    let case = Case::new();
    case.done(REPO);
    let approved = case.asked(&Asker::Agent(4), GitHubRule::NoForcePush, true);
    let applying = case.asked(&Asker::Agent(5), GitHubRule::TagProtection, true);
    assert_eq!(case.answer(approved, vec![0]), Settlement::Settled);
    assert_eq!(case.answer(applying, vec![0]), Settlement::Settled);
    assert_eq!(case.statuses(), ["approved", "approved"]);
    case.db
        .execute(
            "UPDATE github_rule_requests SET status='applying' WHERE id=(SELECT MAX(id) FROM github_rule_requests)",
            [],
        )
        .unwrap();
    let tx = Transaction::new_unchecked(&case.db, TransactionBehavior::Immediate).unwrap();
    assert_eq!(start(&tx).unwrap(), 2);
    tx.commit().unwrap();
    assert_eq!(case.statuses(), ["refused", "refused"]);
    let mut calls = 0;
    let mut set = |_: &Path, rule: GitHubRule, on: bool| {
        calls += 1;
        Ok(page(rule, on, GitHubRuleState::Applied))
    };
    assert_eq!(apply_approved(&case.db, &mut set).unwrap(), 0);
    assert_eq!(calls, 0);
    let told = case.told();
    assert_eq!(told.len(), 2, "{told:?}");
    for text in &told {
        assert!(text.ends_with(": refused."), "{text}");
    }
}

/// DEC-182 S7: an outcome the full team conversation cannot take stays untold on the
/// proposal and is told by the next pass once the conversation has room.
#[test]
fn an_outcome_a_full_conversation_cannot_take_is_told_at_the_next_pass() {
    let case = Case::new();
    case.done(REPO);
    let tx = Transaction::new_unchecked(&case.db, TransactionBehavior::Immediate).unwrap();
    for at in 0..crate::team::MESSAGES_WAITING_MAX {
        let appended = crate::team::append_message(
            &tx,
            &case.project,
            crate::team::MessageAuthor::Owner,
            &format!("waiting {at}"),
            crate::team::MessageState::Pending,
            at,
        )
        .unwrap();
        assert!(appended.is_ok());
    }
    tx.commit().unwrap();
    let question = case.asked(&Asker::Agent(4), GitHubRule::WikiOff, false);
    assert_eq!(case.answer(question, vec![1]), Settlement::Settled);
    assert_eq!(case.statuses(), ["declined"]);
    assert!(case.told().is_empty());
    case.db
        .execute(
            "UPDATE team_messages SET state='answered' WHERE project=?1",
            [PROJECT],
        )
        .unwrap();
    let mut set =
        |_: &Path, rule: GitHubRule, on: bool| Ok(page(rule, on, GitHubRuleState::Applied));
    assert_eq!(apply_approved(&case.db, &mut set).unwrap(), 0);
    let told = case.told();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].ends_with(": declined."), "{}", told[0]);
    assert_eq!(apply_approved(&case.db, &mut set).unwrap(), 0);
    assert_eq!(case.told().len(), 1, "an outcome is told once");
}

/// DEC-182 S7: a reason with a hidden character, such as a direction override or a
/// zero-width space, is refused before any question, so the owner reads only what shows.
#[test]
fn a_reason_with_hidden_characters_is_refused_before_any_question() {
    let case = Case::new();
    case.done(REPO);
    for why in [
        "Docs\u{202E}txt",
        "Docs\u{200B} live here",
        "Docs\u{2066}here",
    ] {
        let refused = request(
            &case.db,
            &case.project,
            &Asker::Agent(4),
            GitHubRule::WikiOff,
            true,
            why,
        );
        assert_eq!(
            refused,
            Err("github_rule_why_invalid".to_owned()),
            "{why:?}"
        );
    }
    assert!(case.questions().is_empty());
}

/// DEC-182 S7: the proposer hears who gets the outcome: a master gets the one message
/// itself, while a sub-agent hears that the message goes to its master, since the outcome
/// is told in the project's team conversation.
#[test]
fn the_proposer_hears_who_gets_the_outcome() {
    let case = Case::new();
    case.done(REPO);
    let (said, refused) = crate::team::tool::questions::propose_reply(
        &case.db,
        &case.project,
        &Asker::Master(7),
        GitHubRule::WikiOff,
        true,
        WHY,
    );
    assert!(!refused, "{said}");
    assert!(
        said.contains("You get one message saying only whether"),
        "{said}"
    );
    let (said, refused) = crate::team::tool::questions::propose_reply(
        &case.db,
        &case.project,
        &Asker::Agent(4),
        GitHubRule::NoForcePush,
        true,
        WHY,
    );
    assert!(!refused, "{said}");
    assert!(
        said.contains("Your master gets one message saying only whether"),
        "{said}"
    );
    assert!(!said.contains("You get one message"), "{said}");
    assert!(!said.contains(REPO), "{said}");
}

/// RV gh-s7 F1: a proposal is recorded with its question in one transaction, so a store
/// failure after the question is logged leaves neither the proposal nor the question,
/// and the agent may propose that rule again at once.
#[test]
fn a_proposal_whose_question_cannot_be_kept_leaves_no_row_and_no_question() {
    let case = Case::new();
    case.done(REPO);
    assert_eq!(start_store(&case.db).unwrap(), 0);
    case.db
        .execute_batch(
            "CREATE TRIGGER keep_fails BEFORE UPDATE OF question ON github_rule_requests
             BEGIN SELECT RAISE(ABORT, 'keep fails'); END;",
        )
        .unwrap();
    let refused = case.propose(&Asker::Agent(4), GitHubRule::WikiOff, true);
    assert_eq!(
        refused,
        Err("github_rule_requests_unavailable".to_owned()),
        "{refused:?}"
    );
    assert!(case.statuses().is_empty(), "{:?}", case.statuses());
    assert!(case.questions().is_empty());
    case.db.execute_batch("DROP TRIGGER keep_fails;").unwrap();
    case.asked(&Asker::Agent(4), GitHubRule::WikiOff, true);
    assert_eq!(case.statuses(), ["pending"]);
    assert_eq!(case.questions().len(), 1);
}

/// RV gh-s7 F1: a start ends a pending proposal whose question is no longer pending, or
/// was never kept, as refused and tells its agent once, so it holds no slot and never
/// blocks that agent and rule; a pending proposal whose question still waits is left.
#[test]
fn a_start_refuses_and_tells_a_pending_proposal_whose_question_is_gone() {
    let case = Case::new();
    case.done(REPO);
    let waiting = case.asked(&Asker::Agent(4), GitHubRule::WikiOff, true);
    let gone = case.asked(&Asker::Agent(5), GitHubRule::NoForcePush, true);
    case.db
        .execute(
            "DELETE FROM questions_log WHERE id=?1",
            [i64::try_from(gone).unwrap()],
        )
        .unwrap();
    case.db
        .execute(
            "INSERT INTO github_rule_requests(project,asker,rule,turn_on,why,repo,created_ms)
             VALUES (?1,'agent:6','tag_protection',1,'Tags stay.',?2,0)",
            [PROJECT, REPO],
        )
        .unwrap();
    assert_eq!(case.statuses(), ["pending", "pending", "pending"]);
    let tx = Transaction::new_unchecked(&case.db, TransactionBehavior::Immediate).unwrap();
    assert_eq!(start(&tx).unwrap(), 2);
    tx.commit().unwrap();
    assert_eq!(case.statuses(), ["pending", "refused", "refused"]);
    let told = case.told();
    assert_eq!(told.len(), 2, "{told:?}");
    assert!(
        told.iter().all(|text| text.ends_with(": refused.")),
        "{told:?}"
    );
    for text in &told {
        plain_for_the_agent(text);
    }
    assert!(
        case.questions()
            .iter()
            .any(|question| question.id == waiting)
    );
    case.asked(&Asker::Agent(5), GitHubRule::NoForcePush, true);
    let tx = Transaction::new_unchecked(&case.db, TransactionBehavior::Immediate).unwrap();
    assert_eq!(start(&tx).unwrap(), 0);
    tx.commit().unwrap();
    assert_eq!(case.told().len(), 2);
}
