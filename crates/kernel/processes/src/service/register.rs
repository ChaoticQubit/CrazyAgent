use super::*;

impl Service {
    /// Undoes a registration that failed after the definition was written: off macOS, where
    /// a failed bootstrap loaded nothing, the unregister steps run best effort; the
    /// definition is removed and systemd reloads best effort, so the next launch installs
    /// again instead of starting a definition no manager runs at login. Input: the
    /// registration error. Output: that error, with any removal failure added.
    pub(super) fn unregistered_after(&self, refused: ServiceError) -> ServiceError {
        if self.platform != Platform::MacOs {
            for command in self.unregister_commands(current_uid()) {
                let _ = run_checked(&command);
            }
        }
        let removed = match std::fs::remove_file(&self.definition) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Some(error),
            _ => None,
        };
        if self.platform == Platform::Linux {
            let reload = Self::command(SYSTEMCTL, &["--user".as_ref(), "daemon-reload".as_ref()]);
            let _ = run_checked(&reload);
        }
        match removed {
            None => refused,
            Some(error) => failure(format!(
                "{}; the definition {} could not be removed: {error}",
                refused.0,
                self.definition.display()
            )),
        }
    }
}
