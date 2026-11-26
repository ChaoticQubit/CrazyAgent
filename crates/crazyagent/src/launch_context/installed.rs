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

impl Installed {
    /// The home a launch's paths name: on Windows the place its install held, read from the
    /// held handle, so a junction above the home repointed later changes nothing the agent is
    /// handed; None off Windows and for a launch that holds no home. Output: the place.
    pub fn home_place(&self) -> Option<&Path> {
        #[cfg(windows)]
        let place = self
            .held
            .as_ref()
            .map(crate::engine::bootstrap::held::Folder::place);
        #[cfg(not(windows))]
        let place = None;
        place
    }

    /// The home a launch's command names: the held place of its install when it has one, as
    /// [`Installed::home_place`] gives it, else the account's home. Inputs: the launch, if
    /// any, and the account's home. Output: the home.
    pub fn home_or<'a>(installed: Option<&'a Self>, home: &'a Path) -> &'a Path {
        installed.and_then(Self::home_place).unwrap_or(home)
    }
}

impl Drop for Installed {
    /// Deletes the launch's files, then releases the home lock on its open file
    /// description, so a process that shares it, such as a child another thread is
    /// starting, can never keep the home busy after the launch ends. Codex skills it cannot
    /// remove stay for the next Codex launch, which refuses to start with them. On Windows
    /// the files go only through the held home, never by path.
    fn drop(&mut self) {
        #[cfg(windows)]
        super::windows::remove_held(self);
        if !cfg!(windows)
            && let Some(files) = &self.files
        {
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
