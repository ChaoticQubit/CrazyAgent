use super::*;

impl InstalledFiles {
    /// Lists the generated file and folder paths a launch deletes by path, all inside its
    /// home; none for a listed harness's private home, whose files go only through a
    /// no-follow walk from the agent home.
    pub(super) fn removals(&self) -> Vec<PathBuf> {
        if self.listed {
            return Vec::new();
        }
        let mut paths = vec![self.home.join(STATE)];
        if self.codex {
            paths.push(self.home.join("config.toml"));
        }
        paths
    }
}

impl Drop for Installed {
    /// Deletes the launch's files, then releases the home lock on its open file
    /// description, so a process that shares it, such as a child another thread is
    /// starting, can never keep the home busy after the launch ends. Codex skills it cannot
    /// remove stay for the next Codex launch, which refuses to start with them.
    fn drop(&mut self) {
        if let Some(files) = &self.files {
            for path in files.removals() {
                remove(&path);
            }
            if files.codex {
                let _ = remove_copies(&files.home);
            }
            for path in &files.rendered {
                crate::harnesses::render::remove(&files.home, path);
            }
        }
        #[cfg(unix)]
        let _ = rustix::fs::flock(&self._lock, FlockOperation::Unlock);
        #[cfg(windows)]
        let _ = crazyagent_win_sandbox::private::unlock(&self._lock);
    }
}
