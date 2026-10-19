use std::io::Write;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Phase {
    Started,
    Finished,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Started => "Started",
            Self::Finished => "Finished",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Failure {
    AlreadyInvalid,
    Window,
    Url,
    Sequence,
    Clock,
    Overlap,
    UnmatchedFinish,
}

impl Failure {
    fn name(self) -> &'static str {
        match self {
            Self::AlreadyInvalid => "AlreadyInvalid",
            Self::Window => "Window",
            Self::Url => "Url",
            Self::Sequence => "Sequence",
            Self::Clock => "Clock",
            Self::Overlap => "Overlap",
            Self::UnmatchedFinish => "UnmatchedFinish",
        }
    }
}

fn admitted_url(url: &str) -> bool {
    matches!(
        url,
        "tauri://localhost"
            | "tauri://localhost/"
            | "tauri://localhost/index.html"
            | "http://tauri.localhost/"
            | "http://tauri.localhost/index.html"
    )
}

struct Observation {
    run: String,
    pid: u32,
    app_started_ns: u128,
    sequence: u16,
    generation: u16,
    pending: Option<String>,
    elapsed_ns: u128,
    invalid: bool,
}

impl Observation {
    fn new(run: String, pid: u32, app_started_ns: u128) -> Result<Self, &'static str> {
        if run.is_empty()
            || run.len() > 64
            || !run
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            || pid == 0
            || app_started_ns == 0
            || app_started_ns >= 10_u128.pow(24)
        {
            return Err("invalid observation identity");
        }
        Ok(Self {
            run,
            pid,
            app_started_ns,
            sequence: 0,
            generation: 0,
            pending: None,
            elapsed_ns: 0,
            invalid: false,
        })
    }

    fn record(
        &mut self,
        window: &str,
        url: &str,
        phase: Phase,
        elapsed_ns: u128,
        wall_ns: u128,
    ) -> Result<String, Failure> {
        if self.invalid {
            return Err(Failure::AlreadyInvalid);
        }
        let result = self.transition(window, url, phase, elapsed_ns, wall_ns);
        if result.is_err() {
            self.invalid = true;
        }
        result
    }

    fn transition(
        &mut self,
        window: &str,
        url: &str,
        phase: Phase,
        elapsed_ns: u128,
        wall_ns: u128,
    ) -> Result<String, Failure> {
        if window != "main" {
            return Err(Failure::Window);
        }
        if !admitted_url(url) {
            return Err(Failure::Url);
        }
        if self.sequence >= 128 {
            return Err(Failure::Sequence);
        }
        if elapsed_ns < self.elapsed_ns
            || elapsed_ns >= 10_u128.pow(24)
            || wall_ns == 0
            || wall_ns >= 10_u128.pow(24)
        {
            return Err(Failure::Clock);
        }
        match phase {
            Phase::Started => {
                if self.pending.is_some() {
                    return Err(Failure::Overlap);
                }
                self.generation += 1;
                self.pending = Some(url.to_owned());
            }
            Phase::Finished => {
                if self.pending.as_deref() != Some(url) {
                    return Err(Failure::UnmatchedFinish);
                }
                self.pending = None;
            }
        }
        self.sequence += 1;
        self.elapsed_ns = elapsed_ns;
        assert!(self.generation > 0);
        assert!(self.sequence <= 128);
        let phase = phase.name();
        let record = format!(
            concat!(
                "{{\"schema\":1,\"run\":\"{}\",\"pid\":{},\"app_started_ns\":\"{}\",",
                "\"sequence\":{},\"generation\":{},\"phase\":\"{}\",\"window\":\"main\",",
                "\"url\":\"{}\",\"elapsed_ns\":\"{}\",\"wall_ns\":\"{}\"}}"
            ),
            self.run,
            self.pid,
            self.app_started_ns,
            self.sequence,
            self.generation,
            phase,
            url,
            elapsed_ns,
            wall_ns
        );
        assert!(record.len() <= 768);
        Ok(record)
    }

    fn failure_record(&self, reason: Failure, phase: Phase, window: &str, url: &str) -> String {
        assert!(self.invalid);
        assert!(self.sequence <= 128);
        assert!(self.generation <= 64);
        let record = format!(
            concat!(
                "{{\"schema\":1,\"run\":\"{}\",\"pid\":{},\"app_started_ns\":\"{}\",",
                "\"invalid\":true,\"reason\":\"{}\",\"phase\":\"{}\",",
                "\"admitted_window\":{},\"admitted_url\":{},",
                "\"sequence\":{},\"generation\":{},\"pending\":{}}}"
            ),
            self.run,
            self.pid,
            self.app_started_ns,
            reason.name(),
            phase.name(),
            window == "main",
            admitted_url(url),
            self.sequence,
            self.generation,
            self.pending.is_some()
        );
        assert!(record.len() <= 768);
        record
    }
}

pub(crate) struct NativeObserver {
    state: Mutex<Observation>,
    started: Instant,
}

impl NativeObserver {
    pub(crate) fn from_environment() -> Option<Self> {
        let run = std::env::var_os("CRAZYAGENT_NATIVE_REFRESH_OBSERVE")?;
        let run = run
            .into_string()
            .expect("native observation run must be UTF8");
        let wall_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("native observation clock before epoch")
            .as_nanos();
        let state = Observation::new(run, std::process::id(), wall_ns)
            .expect("invalid native observation opt-in");
        Some(Self {
            state: Mutex::new(state),
            started: Instant::now(),
        })
    }

    pub(crate) fn observe(&self, window: &str, url: &str, phase: Phase) {
        let mut state = self
            .state
            .lock()
            .expect("native observation mutex poisoned");
        if state.invalid {
            return;
        }
        let wall_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("native observation clock before epoch")
            .as_nanos();
        let record = state
            .record(
                window,
                url,
                phase,
                self.started.elapsed().as_nanos(),
                wall_ns,
            )
            .unwrap_or_else(|reason| state.failure_record(reason, phase, window, url));
        assert!(record.len() <= 768);
        let line = format!("[Tauri:Backend] native-refresh-page: {record}\n");
        let mut stderr = std::io::stderr().lock();
        stderr
            .write_all(line.as_bytes())
            .expect("native observation write failed");
        stderr.flush().expect("native observation flush failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_origin_callbacks_form_an_exact_started_finished_pair() {
        let mut state = state();
        let started = state
            .record("main", "tauri://localhost", Phase::Started, 1, 2)
            .unwrap();
        let finished = state
            .record("main", "tauri://localhost", Phase::Finished, 2, 3)
            .unwrap();
        assert!(started.contains("\"url\":\"tauri://localhost\""));
        assert!(started.contains("\"phase\":\"Started\""));
        assert!(finished.contains("\"url\":\"tauri://localhost\""));
        assert!(finished.contains("\"phase\":\"Finished\""));
        assert!(finished.contains("\"sequence\":2,\"generation\":1"));
        assert_eq!(state.sequence, 2);
        assert_eq!(state.generation, 1);
        assert!(state.pending.is_none());
        assert!(!state.invalid);
    }

    #[test]
    fn bare_origin_does_not_admit_suffix_host_or_path_variants() {
        for url in [
            "tauri://localhost?private-query",
            "tauri://localhost#private-fragment",
            "tauri://localhost/?private-query",
            "tauri://localhost/#private-fragment",
            "tauri://foreign",
            "tauri://localhost.evil",
            "tauri://user@localhost",
            "tauri://localhost:1",
            "tauri://localhost/other",
            "tauri://localhost//",
            "tauri://LOCALHOST",
            "TAURI://localhost",
        ] {
            for phase in [Phase::Started, Phase::Finished] {
                let mut state = state();
                assert_eq!(state.record("main", url, phase, 1, 2), Err(Failure::Url));
                assert_eq!(state.sequence, 0);
                assert_eq!(state.generation, 0);
                assert!(state.pending.is_none());
                assert!(state.invalid);
            }
        }
    }

    #[test]
    fn rejected_callback_diagnostics_are_bounded_and_do_not_copy_external_payload() {
        for (window, url, reason, admitted_window, admitted_asset) in [
            ("private-window", "tauri://localhost/", Failure::Window, false, true),
            ("main", "tauri://localhost/?private-query#private-fragment", Failure::Url, true, false),
        ] {
            let mut state = state();
            assert_eq!(state.record(window, url, Phase::Started, 1, 2), Err(reason));
            let record = state.failure_record(reason, Phase::Started, window, url);
            assert!(record.len() <= 768);
            assert!(record.contains(&format!("\"reason\":\"{}\"", reason.name())));
            assert!(record.contains("\"invalid\":true"));
            assert!(record.contains("\"phase\":\"Started\""));
            assert!(record.contains(&format!("\"admitted_window\":{admitted_window}")));
            assert!(record.contains(&format!("\"admitted_url\":{admitted_asset}")));
            assert!(record.contains("\"sequence\":0,\"generation\":0,\"pending\":false"));
            assert!(!record.contains("private"));
            assert!(!record.contains('?'));
            assert!(!record.contains('#'));
            assert!(!record.contains('\n'));
        }
    }

    #[test]
    fn lifecycle_failure_diagnostics_preserve_pending_facts_and_terminal_rejection() {
        let mut state = state();
        state.record("main", "tauri://localhost/", Phase::Started, 1, 2).unwrap();
        let reason = state.record("main", "tauri://localhost/", Phase::Started, 2, 3).unwrap_err();
        assert_eq!(reason, Failure::Overlap);
        let record = state.failure_record(reason, Phase::Started, "main", "tauri://localhost/");
        assert!(record.contains("\"sequence\":1,\"generation\":1,\"pending\":true"));
        assert_eq!(state.record("main", "tauri://localhost/", Phase::Finished, 3, 4), Err(Failure::AlreadyInvalid));
    }

    fn state() -> Observation {
        Observation::new("unit-258".into(), 12, 1).unwrap()
    }

    #[test]
    fn opt_in_is_absent_in_the_cleared_unit_environment() {
        assert!(NativeObserver::from_environment().is_none());
    }

    #[test]
    fn actual_emitter_serializes_a_pair_then_one_terminal_invalid_record() {
        let observer = NativeObserver {
            state: Mutex::new(state()),
            started: Instant::now(),
        };
        observer.observe("main", "tauri://localhost/", Phase::Started);
        observer.observe("main", "tauri://localhost/", Phase::Finished);
        assert_eq!(observer.state.lock().unwrap().sequence, 2);
        observer.observe("other", "tauri://localhost/", Phase::Started);
        observer.observe("main", "tauri://localhost/", Phase::Started);
        assert!(observer.state.lock().unwrap().invalid);
        assert_eq!(observer.state.lock().unwrap().sequence, 2);
    }

    #[test]
    fn serial_commit_finish_has_same_identity_and_new_generation() {
        let mut state = state();
        let first = state
            .record("main", "tauri://localhost/", Phase::Started, 1, 2)
            .unwrap();
        let second = state
            .record("main", "tauri://localhost/", Phase::Finished, 2, 3)
            .unwrap();
        let third = state
            .record("main", "tauri://localhost/", Phase::Started, 3, 4)
            .unwrap();
        assert!(first.contains("\"generation\":1"));
        assert!(second.contains("\"generation\":1"));
        assert!(third.contains("\"generation\":2"));
        assert!(second.contains("\"sequence\":2"));
        assert_eq!(state.pending.as_deref(), Some("tauri://localhost/"));
    }

    #[test]
    fn overlap_invalidates_all_following_events() {
        let mut state = state();
        state
            .record("main", "tauri://localhost/", Phase::Started, 1, 2)
            .unwrap();
        assert!(
            state
                .record("main", "tauri://localhost/", Phase::Started, 2, 3)
                .is_err()
        );
        assert!(
            state
                .record("main", "tauri://localhost/", Phase::Finished, 3, 4)
                .is_err()
        );
        assert!(state.invalid);
    }

    #[test]
    fn finish_without_commit_rejects() {
        assert!(
            state()
                .record("main", "tauri://localhost/", Phase::Finished, 1, 2)
                .is_err()
        );
    }

    #[test]
    fn changed_finish_url_rejects() {
        let mut state = state();
        state
            .record("main", "tauri://localhost/", Phase::Started, 1, 2)
            .unwrap();
        assert!(
            state
                .record(
                    "main",
                    "tauri://localhost/index.html",
                    Phase::Finished,
                    2,
                    3
                )
                .is_err()
        );
    }

    #[test]
    fn window_and_unadmitted_urls_reject() {
        assert!(
            state()
                .record("other", "tauri://localhost/", Phase::Started, 1, 2)
                .is_err()
        );
        assert!(
            state()
                .record("main", "https://localhost/", Phase::Started, 1, 2)
                .is_err()
        );
        assert!(
            state()
                .record("main", "tauri://localhost/?secret", Phase::Started, 1, 2)
                .is_err()
        );
    }

    #[test]
    fn backwards_elapsed_and_out_of_range_times_reject() {
        let mut state = state();
        state
            .record("main", "tauri://localhost/", Phase::Started, 2, 3)
            .unwrap();
        assert!(
            state
                .record("main", "tauri://localhost/", Phase::Finished, 1, 4)
                .is_err()
        );
        assert!(
            super::tests::state()
                .record("main", "tauri://localhost/", Phase::Started, 1, 0)
                .is_err()
        );
        assert!(
            super::tests::state()
                .record(
                    "main",
                    "tauri://localhost/",
                    Phase::Started,
                    10_u128.pow(24),
                    1
                )
                .is_err()
        );
    }

    #[test]
    fn record_count_is_bounded() {
        let mut state = state();
        for tick in 0..64 {
            state
                .record("main", "tauri://localhost/", Phase::Started, tick * 2, 1)
                .unwrap();
            state
                .record(
                    "main",
                    "tauri://localhost/",
                    Phase::Finished,
                    tick * 2 + 1,
                    1,
                )
                .unwrap();
        }
        assert_eq!(state.sequence, 128);
        assert!(
            state
                .record("main", "tauri://localhost/", Phase::Started, 129, 1)
                .is_err()
        );
    }

    #[test]
    fn run_and_process_identity_are_bounded() {
        assert!(Observation::new("".into(), 1, 1).is_err());
        assert!(Observation::new("x".repeat(65), 1, 1).is_err());
        assert!(Observation::new("bad\"run".into(), 1, 1).is_err());
        assert!(Observation::new("valid".into(), 0, 1).is_err());
        assert!(Observation::new("valid".into(), 1, 0).is_err());
        assert!(Observation::new("valid".into(), 1, 10_u128.pow(24)).is_err());
    }

    #[test]
    fn valid_record_is_bounded_and_contains_no_external_payload() {
        let line = Observation::new("x".repeat(64), u32::MAX, 1)
            .unwrap()
            .record(
                "main",
                "http://tauri.localhost/index.html",
                Phase::Started,
                1,
                2,
            )
            .unwrap();
        assert!(line.len() < 768);
        assert!(line.starts_with("{\"schema\":1,"));
        assert!(!line.contains('\n'));
    }
}
