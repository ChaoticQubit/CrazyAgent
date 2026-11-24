use super::*;

/// Runs one App plugins command on the storage worker. A read-only worker refuses it. Saving
/// the turn-off choice stores the config the edited files resolve to, renames the new global
/// settings file into place, and that config replaces the worker's; the page is read from
/// it. A refusal before the rename leaves the file untouched; after it, the worker takes and
/// stores the config the files then resolve to, if they still resolve, and still refuses.
/// Inputs: the database, the worker's config, and whether the worker is read-only, the
/// settings files and the command. Output: what the command answers.
pub(in crate::storage) fn app_plugins(
    db: &Connection,
    config: &mut EffectiveConfig,
    (readonly, sources, command): (bool, Option<&SettingsSources>, crate::app_plugins::Command),
) -> Result<crate::app_plugins::Done, EngineError> {
    if readonly {
        return Err(error("app_plugins_readonly"));
    }
    let command = match command {
        crate::app_plugins::Command::SetWhenOff { when_off } => {
            let sources = sources.ok_or_else(|| error("app_plugin_setting_unavailable"))?;
            ensure_clear(db).map_err(setting_refusal)?;
            let written = sources.write_global(&move |global: &str, project: &str| {
                let (bytes, next) = crate::config::edit_when_turned_off(global, project, when_off)?;
                store_config(db, &next).map_err(|failed| crate::config::ConfigError(failed.0))?;
                Ok((bytes, next))
            });
            *config = match written {
                Ok(next) => next,
                Err(super::global_write::GlobalRefused { refused, adopted }) => {
                    if let Some(adopted) = adopted
                        && store_config(db, &adopted).is_ok()
                    {
                        *config = *adopted;
                    }
                    return Err(setting_refusal(refused));
                }
            };
            crate::app_plugins::Command::List
        }
        command => command,
    };
    crate::app_plugins::execute(db, config, command)
}
