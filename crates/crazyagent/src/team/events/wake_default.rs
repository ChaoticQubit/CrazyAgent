use super::*;

impl Default for WakeRule {
    fn default() -> Self {
        Self {
            question: true,
            finished: true,
            failed: true,
            change_set_ready: true,
            progress: false,
            hourly_cap: HOURLY_CAP_DEFAULT,
        }
    }
}
