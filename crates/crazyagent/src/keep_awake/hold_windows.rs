use super::hold::{Held, Hold, HoldError, REASON, wanted};
use super::state::Want;
use crazyagent_win_sandbox::power::{Request, RequestKind};

/// The Windows idle hold: one power request with the system-required kind set, and the
/// display-required kind while the display stays on. Windows drops the request when its
/// handle closes, so it ends with the engine process.
#[derive(Debug, Default)]
pub(super) struct WindowsHold {
    request: Option<Request>,
    display: bool,
}

/// Creates the power request and sets its system-required kind. Output: the request, or
/// `Unavailable` with the step that failed.
fn take() -> Result<Request, HoldError> {
    let request =
        Request::new(REASON).map_err(|problem| HoldError::unavailable("power request", problem))?;
    request
        .set(RequestKind::System)
        .map_err(|problem| HoldError::unavailable("system request", problem))?;
    Ok(request)
}

impl Hold for WindowsHold {
    fn apply(&mut self, want: &Want) -> Result<Held, HoldError> {
        let Some(display) = wanted(want) else {
            self.release();
            return Ok(Held::NONE);
        };
        let request = match self.request.take() {
            Some(request) => request,
            None => {
                self.display = false;
                take()?
            }
        };
        if display != self.display {
            let changed = if display {
                request.set(RequestKind::Display)
            } else {
                request.clear(RequestKind::Display)
            };
            changed.map_err(|problem| HoldError::unavailable("display request", problem))?;
            self.display = display;
        }
        self.request = Some(request);
        assert_eq!(self.display, display);
        Ok(Held {
            idle: true,
            lid: false,
            display,
        })
    }

    fn release(&mut self) {
        self.request = None;
        self.display = false;
        assert!(self.request.is_none());
    }
}
