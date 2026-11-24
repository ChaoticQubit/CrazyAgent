use super::*;

impl SettingsSources {
    /// Writes one plain key edit into the global file outside the journal, for a setting
    /// no database row follows: the edit runs on the live texts, the result is staged as a
    /// private file in the global file's own folder, and it is renamed over the global file
    /// only while both files still read as they did. A linked, shared or hard-linked file
    /// and a text that does not resolve are refused and left as they are. Input: the edit,
    /// from the global and project texts to the new global text and its config. Output: the
    /// config the files now resolve to.
    pub(super) fn write_global(
        &self,
        edit: &GlobalEdit<'_>,
    ) -> Result<EffectiveConfig, EngineError> {
        #[cfg(unix)]
        {
            self.global.verify()?;
            match rustix::fs::statat(
                &self.global.parent,
                &self.global.name,
                AtFlags::SYMLINK_NOFOLLOW,
            ) {
                Ok(named) if named.st_mode & 0o170000 == 0o120000 => {
                    return Err(error("settings_metadata_invalid"));
                }
                Ok(_) | Err(rustix::io::Errno::NOENT) => {}
                Err(e) => return Err(error(e)),
            }
        }
        let old = self.snapshot()?;
        let (bytes, next) =
            edit(&old.global.bytes, &old.project.bytes).map_err(|refused| error(refused.0))?;
        if bytes.len() > crate::config::CONFIG_BYTES_MAX {
            return Err(error("settings_file_bounds"));
        }
        self.remove_temporary(SETTING_STAGED)?;
        let name = OsString::from(SETTING_STAGED);
        #[cfg(unix)]
        let mut file = std::fs::File::from(
            rustix::fs::openat(
                &self.global.parent,
                &name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(error)?,
        );
        #[cfg(windows)]
        let mut file = private::file(&self.global.path.join(&name), true).map_err(refused)?;
        let staged = file
            .write_all(bytes.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(error);
        drop(file);
        let published = staged.and_then(|()| {
            let read = self.global.read_name(&name)?;
            if read.identity.is_none() || read.bytes != bytes {
                return Err(error("settings_temporary_conflict"));
            }
            if self.snapshot()? != old {
                return Err(error("settings_source_conflict"));
            }
            #[cfg(unix)]
            rustix::fs::renameat(
                &self.global.parent,
                &name,
                &self.global.parent,
                &self.global.name,
            )
            .map_err(error)?;
            #[cfg(windows)]
            std::fs::rename(
                self.global.path.join(&name),
                self.global.path.join(&self.global.name),
            )
            .map_err(error)?;
            Ok(read.identity)
        });
        let staged_identity = match published {
            Ok(identity) => identity,
            Err(refused) => {
                let _ = self.remove_temporary(SETTING_STAGED);
                return Err(refused);
            }
        };
        self.global.sync_parent()?;
        let now = self.snapshot()?;
        if now.global.identity != staged_identity
            || now.global.bytes != bytes
            || now.project != old.project
        {
            return Err(error("settings_publication_unknown"));
        }
        let resolved = now.config()?;
        assert_eq!(resolved.id, next.id);
        Ok(resolved)
    }
}

/// One plain key edit of the global settings text: from the global and project texts to
/// the new global text and the config both then resolve to, or the config refusal.
pub(super) type GlobalEdit<'a> =
    dyn Fn(&str, &str) -> Result<(String, EffectiveConfig), crate::config::ConfigError> + 'a;

/// The name a plain key edit is staged under in the global file's folder before its rename.
pub(super) const SETTING_STAGED: &str = ".crazyagent-setting-staged";
