use crate::engine::owner::{OwnerOperation, OwnerRequest};
use crate::platform::unix_socket::UnixStream;
use crate::protocol::RequestId;
use std::cell::RefCell;
use std::future::Future;
use tokio::io::AsyncWriteExt;

const KIND_BYTES_MAX: usize = 64;

tokio::task_local! {
    static TRACE: RefCell<Trace>;
}

/// What one owner exchange did, kept for the line the `owner-trace` feature prints when the
/// exchange ends: request id, operation kind, whether its id was already running, how the
/// request ended and whether its reply frame was written. Fields hold only the id, which the
/// protocol checks is short safe ASCII, and fixed words; never a payload, path or name.
#[derive(Default)]
struct Trace {
    id: Option<String>,
    op: Option<String>,
    claim: Option<&'static str>,
    end: Option<&'static str>,
    reply: Option<&'static str>,
}

impl Trace {
    /// The exchange's one line, with `-` for each part the exchange never reached, so a
    /// reader sees where it stopped. Input: the trace.
    /// Output: `owner_trace id=.. op=.. claim=.. end=.. reply=..`.
    fn line(&self) -> String {
        format!(
            "owner_trace id={} op={} claim={} end={} reply={}",
            self.id.as_deref().unwrap_or("-"),
            self.op.as_deref().unwrap_or("-"),
            self.claim.unwrap_or("-"),
            self.end.unwrap_or("-"),
            self.reply.unwrap_or("-"),
        )
    }
}

impl Drop for Trace {
    /// Prints the line once the exchange ends, cut off or not, when the feature is on and
    /// the exchange named an owner request id; other protocols and the runner print nothing.
    /// Input: the trace. Output: none.
    fn drop(&mut self) {
        if cfg!(feature = "owner-trace") && self.id.is_some() {
            eprintln!("{}", self.line());
        }
    }
}

/// Changes the trace of the exchange this task serves. Outside one, such as a busy
/// refusal, a job or a build without the feature, nothing is recorded and nothing fails.
/// Input: the change. Output: none.
fn note(change: impl FnOnce(&mut Trace)) {
    let _ = TRACE.try_with(|trace| change(&mut trace.borrow_mut()));
}

/// Sets one part of the exchange's trace when dropped, so a future cut off by a deadline or
/// a panic before its end still leaves its mark; a whole future swaps the word first.
/// Fields: the setter and the word it writes.
struct Mark {
    set: fn(&mut Trace, &'static str),
    word: &'static str,
}

impl Drop for Mark {
    fn drop(&mut self) {
        let (set, word) = (self.set, self.word);
        note(|trace| set(trace, word));
    }
}

/// Serves one owner connection as one traced exchange when the feature is on (and in this
/// crate's tests, which print nothing); otherwise it only serves it. Input: the connection's
/// whole service. Output: none; the line is printed as the trace drops.
pub(crate) async fn connection(served: impl Future<Output = ()>) {
    if cfg!(any(test, feature = "owner-trace")) {
        TRACE.scope(RefCell::new(Trace::default()), served).await;
    } else {
        served.await;
    }
}

/// Records the owner request this exchange serves: its id and its operation's kind,
/// `Apply/<mutation>`, `Read/<query>` or `Reconcile`, read from the serde tags alone.
/// Input: the request. Output: the same request.
pub(crate) fn request(request: &OwnerRequest) -> &OwnerRequest {
    note(|trace| {
        trace.id = Some(request.request_id.as_str().to_owned());
        trace.op = Some(kind(&request.operation));
    });
    request
}

/// The kind of an owner operation from its serde `kind` tags, each a variant name; a tag
/// that is not one short alphanumeric word reads `?`. Input: the operation. Output: the kind.
fn kind(operation: &OwnerOperation) -> String {
    let tags = serde_json::to_value(operation).unwrap_or_default();
    let tag = |value: &serde_json::Value| {
        value
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .filter(|tag| {
                tag.len() <= KIND_BYTES_MAX && tag.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .unwrap_or("?")
            .to_owned()
    };
    match operation {
        OwnerOperation::Apply { .. } => format!("Apply/{}", tag(&tags["mutation"])),
        OwnerOperation::Read { .. } => format!("Read/{}", tag(&tags["query"])),
        OwnerOperation::Reconcile { .. } => "Reconcile".to_owned(),
    }
}

/// Records a projects-home run claim for `id`, naming the exchange by it when no request
/// did: `hit` when the id was already running, so the claim was refused, `miss` when this
/// exchange took it. Inputs: the claimed id and the claim. Output: the same claim.
pub(crate) fn claim<T>(id: &RequestId, claim: Option<T>) -> Option<T> {
    note(|trace| {
        trace.id.get_or_insert_with(|| id.as_str().to_owned());
        trace.claim = Some(if claim.is_some() { "miss" } else { "hit" });
    });
    claim
}

/// Records a Reconcile's check of the running set: `hit` when the asked id is still
/// running, `miss` when it is not. Input: the check. Output: the same check.
pub(crate) fn held(running: bool) -> bool {
    note(|trace| trace.claim = Some(if running { "hit" } else { "miss" }));
    running
}

/// Records how the request part of an exchange ended: `served`, `deferred` to its own
/// bounds, `error` (its code is not kept: an io error's text can hold a path) or `cut` when
/// dropped unfinished by the deadline or a panic. Input: the request. Output: its result.
pub(crate) async fn ended<T, E>(
    request: impl Future<Output = Result<Option<T>, E>>,
) -> Result<Option<T>, E> {
    let mut mark = Mark {
        set: |trace, word| trace.end = Some(word),
        word: "cut",
    };
    let result = request.await;
    mark.word = match &result {
        Ok(None) => "served",
        Ok(Some(_)) => "deferred",
        Err(_) => "error",
    };
    result
}

/// Records one reply frame write as `done` (`written` for a reply, `refused` for a
/// refusal), or `failed` when it errs or is cut off before its end.
/// Inputs: the write and the word for a whole write. Output: the write's result.
pub(crate) async fn reply<E>(
    write: impl Future<Output = Result<(), E>>,
    done: &'static str,
) -> Result<(), E> {
    let mut mark = Mark {
        set: |trace, word| trace.reply = Some(word),
        word: "failed",
    };
    let result = write.await;
    if result.is_ok() {
        mark.word = done;
    }
    result
}

/// Writes one refusal frame to the client and records it `refused`, or `failed` when the
/// write errs or is cut off ([`reply`]). Inputs: the connection and the refusal frame.
/// Output: the write's result.
pub(crate) async fn refused(stream: &mut UnixStream, frame: &[u8]) -> std::io::Result<()> {
    reply(stream.write_all(frame), "refused").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::owner::OwnerMutation;
    use crate::protocol::{Cursor, PROTOCOL_VERSION};
    use std::time::Duration;

    const PLANTED: &str = "sk-planted-7f3a9c";

    fn asked() -> OwnerRequest {
        OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("home-1").unwrap(),
            expected_revision: Cursor::new(1),
            operation: OwnerOperation::Apply {
                mutation: OwnerMutation::SetProjectsHome {
                    path: format!("/home/planted-owner/{PLANTED}"),
                },
            },
        }
    }

    async fn traced(exchange: impl Future<Output = ()>) -> String {
        TRACE
            .scope(RefCell::new(Trace::default()), async {
                exchange.await;
                TRACE.with(|trace| trace.borrow().line())
            })
            .await
    }

    #[tokio::test]
    async fn an_exchange_line_names_its_request_and_never_its_payload() {
        let asked = asked();
        assert!(
            held(true),
            "outside an exchange a hook only passes its value on"
        );
        let line = traced(async {
            request(&asked);
            claim(&asked.request_id, None::<()>);
            let _ = ended(async { Ok::<Option<()>, ()>(None) }).await;
            reply(async { Ok::<(), ()>(()) }, "written").await.unwrap();
        })
        .await;
        assert_eq!(
            line,
            "owner_trace id=home-1 op=Apply/SetProjectsHome claim=hit end=served reply=written"
        );
        assert!(
            !line.contains(PLANTED) && !line.contains("planted-owner"),
            "{line}"
        );
    }

    #[tokio::test]
    async fn a_write_or_request_cut_off_before_its_end_is_marked() {
        let asked = asked();
        let line = traced(async {
            request(&asked);
            let write = reply(std::future::pending::<Result<(), ()>>(), "written");
            assert!(tokio::time::timeout(Duration::ZERO, write).await.is_err());
            let cut = ended(std::future::pending::<Result<Option<()>, ()>>());
            assert!(tokio::time::timeout(Duration::ZERO, cut).await.is_err());
        })
        .await;
        assert_eq!(
            line,
            "owner_trace id=home-1 op=Apply/SetProjectsHome claim=- end=cut reply=failed"
        );
    }
}
