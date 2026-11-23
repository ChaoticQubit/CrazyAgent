use super::*;

/// Runs one App plugins command on the storage worker. A read-only worker refuses it. Saving
/// the turn-off choice writes the global settings file first, then the config the files
/// resolve to replaces the worker's and is stored, and the page is read from it. Inputs: the
/// database, the worker's config, and whether the worker is read-only, the settings files
/// and the command. Output: what the command answers.
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
            let next = sources
                .write_global(&move |global: &str, project: &str| {
                    crate::config::edit_when_turned_off(global, project, when_off)
                })
                .map_err(setting_refusal)?;
            store_config(db, &next)?;
            *config = next;
            crate::app_plugins::Command::List
        }
        command => command,
    };
    crate::app_plugins::execute(db, config, command)
}
