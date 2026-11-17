use std::sync::atomic::{AtomicBool, Ordering};

/// Set once this app run has taken its one replacement of an older engine, so a swap that
/// does not end the mismatch never turns into a restart loop (DEC-134 addendum 1).
pub(crate) static ENGINE_REPLACED: AtomicBool = AtomicBool::new(false);

/// Set when that replacement succeeded and no connect reply has said so yet.
pub(crate) static ENGINE_UPDATE_UNTOLD: AtomicBool = AtomicBool::new(false);

/// Whether an engine reporting `engine` is older than an app at `app`, so the app may replace
/// it on its own at launch (DEC-134 addendum 1). Inputs: both versions. Output: true only
/// when both read as versions and the app's is newer; equal, newer and unreadable give false.
pub(crate) fn engine_older(engine: &str, app: &str) -> bool {
    crazyagent::update::is_newer(app, engine).unwrap_or(false)
}

/// Whether the launch may replace the engine that answered without the owner's confirm: it
/// is older than this app and the login service is installed and running, so the swap stops
/// the service's engine and never installs a service beside an engine started by hand
/// (DEC-134 addendum 1, #111 F1). Inputs: both versions and the service's status. Output:
/// the answer.
pub(crate) fn swap_on_launch(
    engine: &str,
    app: &str,
    service: &crazyagent::service::Status,
) -> bool {
    service.installed && service.running && engine_older(engine, app)
}

/// Takes this app run's one replacement of an older engine. Input: the run's flag. Output:
/// true the first time, false after.
pub(crate) fn first_replacement(taken: &AtomicBool) -> bool {
    !taken.swap(true, Ordering::SeqCst)
}

/// Takes a one-shot signal: true once after it was set, then false until it is set again.
/// Input: the signal. Output: whether it was set.
pub(crate) fn take_signal(signal: &AtomicBool) -> bool {
    signal.swap(false, Ordering::SeqCst)
}

/// Whether this app replaced an older engine since the last connect reply said so. Reading
/// it clears it, so the owner is told once. Output: the answer.
pub(crate) fn engine_update_untold() -> bool {
    take_signal(&ENGINE_UPDATE_UNTOLD)
}
