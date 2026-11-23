use super::grants_store::{data_root, log, requests_table};
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use crate::questions::{Ask, Asker, EngineSource, QuestionKind};
use kernel_sandbox::grants::{
    ALLOW, DONT_ALLOW, REASON_BYTES_MAX, Requested, SOCKET_REFUSED, asked_meaning, capital,
    checked, plain_reason, request_id, socket_identity,
};
use kernel_sandbox::policy::{AccessGrant, AgentAccess};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::Path;

/// The most access requests one project has waiting for the owner at once.
pub const REQUESTS_PENDING_MAX: i64 = 4;
#[cfg(unix)]
const IDE_FOLDERS_SCANNED_MAX: usize = 256;
const _: () = assert!(REASON_BYTES_MAX < crate::questions::CONTEXT_BYTES_MAX / 2);

/// The loopback ports the engine recorded for its projects' IDE servers and their proxies,
/// read from at most `IDE_FOLDERS_SCANNED_MAX` project folders. Input: the engine's data
/// folder. Output: the ports, none when there is no IDE folder.
#[cfg(unix)]
pub(crate) fn ide_ports(data_root: &Path) -> Vec<u16> {
    let Ok(folders) = std::fs::read_dir(data_root.join(crate::ide::FOLDER)) else {
        return Vec::new();
    };
    let ports: Vec<u16> = folders
        .take(IDE_FOLDERS_SCANNED_MAX)
        .filter_map(Result::ok)
        .filter_map(|entry| crate::ide::ports::recorded(&entry.path()))
        .flat_map(|ports| [ports.server, ports.proxy])
        .collect();
    assert!(ports.len() <= 2 * IDE_FOLDERS_SCANNED_MAX);
    ports
}

#[cfg(not(unix))]
pub(crate) fn ide_ports(_data_root: &Path) -> Vec<u16> {
    Vec::new()
}

/// Turns an agent's access request into a real owner question (F08): the grant is checked
/// as a save checks it, one the policy already holds asks nothing, and otherwise the engine
/// raises a question with the options [`ALLOW`] and [`DONT_ALLOW`], the words written by the
/// engine from the checked grant and the agent's reason only as quoted context. A socket's
/// request keeps the identity of the socket file the question names, so the Allow binds to
/// it ([`decide`]). Inputs: the engine connection, the project, who asked, the grant and
/// the reason. Output: the outcome, or a plain refusal code.
pub fn request(
    db: &Connection,
    project: &ProjectId,
    asker: &Asker,
    grant: &AccessGrant,
    reason: &str,
) -> Result<Requested, String> {
    let reason = plain_reason(reason)?;
    let data = data_root(db).ok_or("grant_requests_unavailable")?;
    let grant =
        checked(&super::GRANT_HOOKS, grant, &data).map_err(|code| asked_refusal(grant, code))?;
    let identity = match &grant {
        AccessGrant::Socket { path } => Some(socket_identity(path).ok_or(SOCKET_REFUSED)?),
        AccessGrant::Port { .. } | AccessGrant::TempFolder => None,
    };
    let policy = crate::sandbox::policy_store::load(db, project.as_str())
        .map_err(|_| "grant_requests_unavailable")?;
    let full_reach = policy.access == AgentAccess::Full && grant != AccessGrant::TempFolder;
    if full_reach || policy.grants.contains(&grant) {
        return Ok(Requested::AlreadyAllowed);
    }
    let mut grown = policy.clone();
    grown.grants.push(grant.clone());
    if grown.validate().is_err() {
        return Err("grant_full".into());
    }
    let settings = crate::sandbox_settings::load(db).ok();
    let calls = settings.map(|settings| settings.linux_supervisor);
    let host = crate::sandbox::linux_host_kept().map(|host| host.under(calls));
    let sockets = settings.map(|settings| settings.linux_sockets);
    let gap =
        kernel_sandbox::policy::linux_network_gap(policy.network, host, sockets, &grown.grants);
    let body = serde_json::to_string(&grant).map_err(|_| "grant_requests_unavailable")?;
    requests_table(db).map_err(|_| "grant_requests_unavailable")?;
    let waiting: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM access_requests WHERE project=?1 AND status='pending'",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(|_| "grant_requests_unavailable")?;
    if waiting >= REQUESTS_PENDING_MAX {
        return Err("grant_requests_full".into());
    }
    db.execute(
        "INSERT INTO access_requests(project,asker,grant_json,reason,created_ms,identity) VALUES (?1,?2,?3,?4,?5,?6)",
        params![project.as_str(), asker.name(), body, &reason, crate::team::now_ms(), identity],
    )
    .map_err(|_| "grant_requests_unavailable")?;
    let id = db.last_insert_rowid();
    assert!(id > 0);
    let ask = Ask {
        question: format!(
            "{} asks for one extra: {}. Allow it?",
            capital(&crate::questions::asker_label(&asker.name())),
            grant.label()
        ),
        kind: QuestionKind::Single,
        options: vec![ALLOW.to_owned(), DONT_ALLOW.to_owned()],
        recommended: Some(DONT_ALLOW.to_owned()),
        context: Some(format!(
            "{} Its reason, in its own words: {}. Allow only if you trust it. Allowing adds it to this project's Agent access, where you can remove it; running agents restart at their next pause to use it.",
            asked_meaning(&grant, gap.as_deref()),
            serde_json::to_string(&reason).map_err(|_| "grant_requests_unavailable")?
        )),
    };
    let source = EngineSource::Access(id.to_string());
    match crate::questions::raise(db, project, &source, &ask) {
        Ok(question) => {
            db.execute(
                "UPDATE access_requests SET question=?2 WHERE id=?1",
                params![id, i64::try_from(question).unwrap_or(i64::MAX)],
            )
            .map_err(|_| "grant_requests_unavailable")?;
            Ok(Requested::Asked(question))
        }
        Err(problem) => {
            let _ = db.execute("DELETE FROM access_requests WHERE id=?1", [id]);
            Err(problem.0)
        }
    }
}

/// The refusal an agent's access request gets for a grant [`checked`] refused. A socket
/// path the agent wrote plainly gets [`SOCKET_REFUSED`] whatever about it failed, since
/// each detailed code tells whether a folder or file outside the agent's sandbox exists
/// and what it is (RV-DS241-87); the detailed code goes to the engine log for the owner. A
/// path that is not plain as written, a socket asked off macOS and every port or temp
/// folder refusal keep their own code, which no file decides. Inputs: the grant as asked
/// and the check's code. Output: the code to answer with.
fn asked_refusal(grant: &AccessGrant, code: &'static str) -> &'static str {
    match grant {
        AccessGrant::Socket { .. } if grant.plain() && code != "grant_socket_needs_macos" => {
            eprintln!("access_request_refused code={code}");
            SOCKET_REFUSED
        }
        AccessGrant::Socket { .. } | AccessGrant::Port { .. } | AccessGrant::TempFolder => code,
    }
}

/// Settles the access request behind an engine question the owner answered or dismissed,
/// in the caller's transaction: an Allow adds the grant to the project's policy with its own
/// versioned owner write when it still passes its check unchanged, as the question showed
/// it, and fits. A socket must still resolve to the same path and be the same socket file
/// the request kept; one swapped for a link or another socket after the question was asked,
/// or asked by an engine that kept no identity, is denied and needs a fresh request
/// (RV-DS241-82). Every outcome is logged. A question of another asker is left alone. Inputs: the transaction, the
/// project, the question's asker, whether the owner allowed it and the time. Output: done,
/// or the store error.
pub(crate) fn decide(
    tx: &Transaction<'_>,
    project: &ProjectId,
    asker: &str,
    allowed: bool,
    now: i64,
) -> Result<(), EngineError> {
    let Some(id) = request_id(asker) else {
        return Ok(());
    };
    let unavailable = |_| failure("grant_requests_unavailable");
    requests_table(tx).map_err(unavailable)?;
    let row: Option<(String, Option<i64>, Option<String>)> = tx
        .query_row(
            "SELECT grant_json,question,identity FROM access_requests WHERE id=?1 AND project=?2 AND status='pending'",
            params![id, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((body, question, identity)) = row else {
        return Ok(());
    };
    let by = question.map_or_else(|| "owner".to_owned(), |q| format!("question {q}"));
    let grant: AccessGrant =
        serde_json::from_str(&body).map_err(|_| failure("grant_requests_unavailable"))?;
    let data = data_root(tx).ok_or_else(|| failure("grant_requests_unavailable"))?;
    let kept = if allowed {
        checked(&super::GRANT_HOOKS, &grant, &data)
            .ok()
            .filter(|now| *now == grant)
            .filter(|now| match now {
                AccessGrant::Socket { path } => {
                    identity.is_some() && socket_identity(path) == identity
                }
                AccessGrant::Port { .. } | AccessGrant::TempFolder => true,
            })
    } else {
        None
    };
    let mut policy =
        crate::sandbox::policy_store::load(tx, project.as_str()).map_err(unavailable)?;
    let granted = match kept {
        Some(kept) if policy.grants.contains(&kept) => true,
        Some(kept) => {
            policy.grants.push(kept);
            if policy.validate().is_ok() {
                let item = crate::storage::versioned::agent_policy_item(project);
                let current = crate::storage::versioned::current(tx, &item)?.version;
                if crate::storage::versioned::owner_write(tx, &item, current)?.is_err() {
                    return Err(failure(crate::storage::versioned::STALE_CODE));
                }
                crate::sandbox::policy_store::save(tx, project.as_str(), &policy)
                    .map_err(unavailable)?;
                true
            } else {
                false
            }
        }
        None => false,
    };
    let (status, action) = if granted {
        ("allowed", "allowed")
    } else {
        ("denied", "denied")
    };
    tx.execute(
        "UPDATE access_requests SET status=?2 WHERE id=?1",
        params![id, status],
    )
    .map_err(unavailable)?;
    log(tx, project.as_str(), action, &grant, &by, now).map_err(unavailable)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests;
