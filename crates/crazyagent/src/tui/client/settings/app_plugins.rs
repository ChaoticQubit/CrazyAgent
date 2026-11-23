use super::*;
use crate::engine::owner::{AppPluginsPage, OwnerData, OwnerRead, PluginUser};
use kernel_plugin_host::grants::PluginGrants;
use kernel_plugin_host::usage::TurnOff;
use kernel_plugin_host::views::Preview;
use std::path::PathBuf;

/// One App plugins request the Settings page sends. A typed key travels only inside
/// `SetKey`, as the engine's secret type, and is never printed.
pub(in crate::tui) enum AppPluginsJob {
    List,
    Grants,
    Preview {
        folder: PathBuf,
    },
    Review {
        name: String,
    },
    Add {
        folder: PathBuf,
        sha256: String,
        replace: bool,
    },
    Trust {
        name: String,
        sha256: String,
    },
    SetEnabled {
        name: String,
        enabled: bool,
        when_off: Option<TurnOff>,
    },
    Users {
        name: String,
    },
    Remove {
        name: String,
    },
    BindAccount {
        name: String,
        secret: String,
        account: String,
    },
    SetKey {
        name: String,
        secret: String,
        key: AccountSecret,
    },
    Unbind {
        name: String,
        secret: String,
    },
}

impl std::fmt::Debug for AppPluginsJob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The engine's answer to one App plugins request. Every plugin change answers with the
/// new page; a folder check and a review answer with a trust screen; a users read answers
/// with the runs holding the named plugin and the stored turn-off choice; the grants read
/// answers with every plugin's granted capabilities and essential mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum AppPluginsEvent {
    Page(Result<AppPluginsPage, String>),
    Preview(Result<Preview, String>),
    Grants(Result<Vec<PluginGrants>, String>),
    Users {
        name: String,
        result: Result<(Vec<PluginUser>, TurnOff), String>,
    },
}

/// The owner setting naming what turning a plugin off does to runs holding it.
const WHEN_OFF_SETTING: &str = "plugins.when_turned_off";

/// The stored turn-off choice from one setting read: the read value, or the default when no
/// settings file sets the plugins group. Input: the read. Output: the choice or the code.
fn when_off(read: Result<OwnerData, crate::engine::EngineError>) -> Result<TurnOff, String> {
    match read {
        Ok(OwnerData::Setting { descriptor }) if descriptor.name == WHEN_OFF_SETTING => {
            serde_json::from_value(descriptor.value).map_err(|_| "unexpected_reply".to_owned())
        }
        Ok(_) => Err("unexpected_reply".into()),
        Err(error) if error.0 == "owner_setting_not_found" => Ok(TurnOff::default()),
        Err(error) => Err(error.0),
    }
}

impl AppPluginsJob {
    /// The name a job is logged and refused under, never its folder text or key. Output:
    /// the name.
    pub(in crate::tui) fn name(&self) -> &'static str {
        match self {
            Self::List => "AppPlugins",
            Self::Grants => "PluginGrants",
            Self::Preview { .. } => "AppPluginPreview",
            Self::Review { .. } => "AppPluginReview",
            Self::Add { .. } => "AppPluginAdd",
            Self::Trust { .. } => "AppPluginTrust",
            Self::SetEnabled { .. } => "AppPluginSetEnabled",
            Self::Users { .. } => "AppPluginUsers",
            Self::Remove { .. } => "AppPluginRemove",
            Self::BindAccount { .. } => "AppPluginBindAccount",
            Self::SetKey { .. } => "AppPluginSetKey",
            Self::Unbind { .. } => "AppPluginUnbind",
        }
    }
}

impl AppPluginsEvent {
    /// The reply a job gets when it fails before or at the engine. Inputs: the job and the
    /// bounded code. Output: the event of the job's kind carrying the code.
    pub(in crate::tui) fn failed(job: &AppPluginsJob, code: String) -> Self {
        match job {
            AppPluginsJob::Preview { .. } | AppPluginsJob::Review { .. } => {
                Self::Preview(Err(code))
            }
            AppPluginsJob::Grants => Self::Grants(Err(code)),
            AppPluginsJob::Users { name } => Self::Users {
                name: name.clone(),
                result: Err(code),
            },
            AppPluginsJob::List
            | AppPluginsJob::Add { .. }
            | AppPluginsJob::Trust { .. }
            | AppPluginsJob::SetEnabled { .. }
            | AppPluginsJob::Remove { .. }
            | AppPluginsJob::BindAccount { .. }
            | AppPluginsJob::SetKey { .. }
            | AppPluginsJob::Unbind { .. } => Self::Page(Err(code)),
        }
    }
}

impl NativeOwner {
    /// Runs one App plugins request through the trusted owner controller. Inputs: the job
    /// and the lost-connection flag. Output: the event; a transport failure sets the flag
    /// so the shell reconnects.
    pub(super) async fn app_plugins(
        &mut self,
        job: AppPluginsJob,
        lost: &mut bool,
    ) -> AppPluginsEvent {
        let controller = match self.settings_controller() {
            Ok(controller) => controller,
            Err(code) => return AppPluginsEvent::failed(&job, code),
        };
        let page = |result: Result<AppPluginsPage, crate::engine::EngineError>, lost: &mut bool| {
            AppPluginsEvent::Page(result.map_err(|error| problem(&error, lost)))
        };
        let preview = |result: Result<Preview, crate::engine::EngineError>, lost: &mut bool| {
            AppPluginsEvent::Preview(result.map_err(|error| problem(&error, lost)))
        };
        match job {
            AppPluginsJob::List => page(controller.app_plugins().await, lost),
            AppPluginsJob::Grants => {
                AppPluginsEvent::Grants(match controller.read(OwnerRead::PluginGrants).await {
                    Ok(reply) => match reply.data {
                        OwnerData::PluginGrants { plugins } => Ok(plugins),
                        _ => Err("unexpected_reply".into()),
                    },
                    Err(error) => Err(problem(&error, lost)),
                })
            }
            AppPluginsJob::Preview { folder } => {
                preview(controller.app_plugin_preview(folder).await, lost)
            }
            AppPluginsJob::Review { name } => {
                preview(controller.app_plugin_review(name).await, lost)
            }
            AppPluginsJob::Add {
                folder,
                sha256,
                replace,
            } => page(
                controller.app_plugin_add(folder, sha256, replace).await,
                lost,
            ),
            AppPluginsJob::Trust { name, sha256 } => {
                page(controller.app_plugin_trust(name, sha256).await, lost)
            }
            AppPluginsJob::SetEnabled {
                name,
                enabled,
                when_off,
            } => page(
                controller
                    .app_plugin_set_enabled(name, enabled, when_off)
                    .await,
                lost,
            ),
            AppPluginsJob::Users { name } => {
                let users = match controller.app_plugin_users(name.clone()).await {
                    Ok(users) => users,
                    Err(error) => {
                        let result = Err(problem(&error, lost));
                        return AppPluginsEvent::Users { name, result };
                    }
                };
                let read = controller
                    .read(OwnerRead::Setting {
                        name: WHEN_OFF_SETTING.into(),
                    })
                    .await
                    .map(|reply| reply.data);
                if let Err(error) = &read
                    && error.0 != "owner_setting_not_found"
                {
                    let result = Err(problem(error, lost));
                    return AppPluginsEvent::Users { name, result };
                }
                let result = when_off(read).map(|choice| (users, choice));
                AppPluginsEvent::Users { name, result }
            }
            AppPluginsJob::Remove { name } => page(controller.app_plugin_remove(name).await, lost),
            AppPluginsJob::BindAccount {
                name,
                secret,
                account,
            } => page(
                controller
                    .app_plugin_bind_account(name, secret, account)
                    .await,
                lost,
            ),
            AppPluginsJob::SetKey { name, secret, key } => {
                page(controller.app_plugin_set_key(name, secret, key).await, lost)
            }
            AppPluginsJob::Unbind { name, secret } => {
                page(controller.app_plugin_unbind(name, secret).await, lost)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_prints_its_name_never_its_key() {
        let job = AppPluginsJob::SetKey {
            name: "harbor".into(),
            secret: "api_key".into(),
            key: AccountSecret::new(b"sk-test-never-printed".to_vec()).unwrap(),
        };
        assert_eq!(format!("{job:?}"), "AppPluginSetKey");
        assert_eq!(
            format!("{:?}", SettingsJob::AppPlugins(Box::new(job))),
            "AppPluginSetKey"
        );
        let users = AppPluginsJob::Users {
            name: "harbor".into(),
        };
        assert_eq!(
            AppPluginsEvent::failed(&users, "trusted_connection_unavailable".into()),
            AppPluginsEvent::Users {
                name: "harbor".into(),
                result: Err("trusted_connection_unavailable".into()),
            }
        );
        let folder = AppPluginsJob::Preview {
            folder: PathBuf::from("/Users/owner/secret-plugin"),
        };
        assert_eq!(format!("{folder:?}"), "AppPluginPreview");
        assert_eq!(
            AppPluginsEvent::failed(&folder, "app_plugin_request_invalid".into()),
            AppPluginsEvent::Preview(Err("app_plugin_request_invalid".into()))
        );
    }

    #[test]
    fn the_turn_off_confirm_reads_the_stored_choice_and_no_file_means_the_default() {
        let setting = |name: &str, value: serde_json::Value| {
            Ok(OwnerData::Setting {
                descriptor: Box::new(crate::config::SettingDescriptor {
                    name: name.into(),
                    default: serde_json::json!("refuse_new_calls"),
                    value,
                    origin: crate::config::Origin::Global,
                    scope: crate::config::SettingScope::Global,
                    effect_timing: crate::config::EffectTiming::Immediate,
                }),
            })
        };
        assert_eq!(
            when_off(setting(WHEN_OFF_SETTING, serde_json::json!("stop_runs"))),
            Ok(TurnOff::StopRuns)
        );
        assert_eq!(
            when_off(setting(
                WHEN_OFF_SETTING,
                serde_json::json!("refuse_new_calls")
            )),
            Ok(TurnOff::RefuseNewCalls)
        );
        assert_eq!(
            when_off(Err(crate::engine::EngineError(
                "owner_setting_not_found".into()
            ))),
            Ok(TurnOff::RefuseNewCalls)
        );
        assert_eq!(
            when_off(Err(crate::engine::EngineError("owner_busy".into()))),
            Err("owner_busy".into())
        );
        assert_eq!(
            when_off(setting(
                "plugins.essential_unlocked",
                serde_json::json!(false)
            )),
            Err("unexpected_reply".into())
        );
        assert_eq!(
            when_off(setting(WHEN_OFF_SETTING, serde_json::json!("pause"))),
            Err("unexpected_reply".into())
        );
    }
}
