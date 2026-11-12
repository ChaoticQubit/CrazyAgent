use super::settings::problem;
use super::*;
use crate::app_plugins::{Listing, Preview};
use crate::engine::owner::{AccountSecret, SearchSaved};
use crate::semantic::{SearchChoice, SearchView};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Most bytes a manifest may hold, the engine's own bound; a longer text is refused whole.
pub(in crate::tui) const MANIFEST_BYTES_MAX: usize = 16 * 1024;

/// One App plugins or Search request the two Settings pages send. A typed key travels only
/// inside `SetKey`, as the engine's secret type, and is never printed.
pub(in crate::tui) enum AppPluginsJob {
    List,
    Preview {
        yaml: String,
    },
    ReadFile {
        path: PathBuf,
    },
    Review {
        name: String,
    },
    Add {
        yaml: String,
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
    Search,
    SetSearch {
        choice: SearchChoice,
        confirm_host: Option<String>,
        version: u64,
    },
    Download {
        confirm_bytes: u64,
    },
    CancelDownload,
    DeleteModel,
}

impl std::fmt::Debug for AppPluginsJob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(job_name(self))
    }
}

/// The engine's answer to one App plugins or Search request. Every plugin change answers
/// with the new listing; a manifest check, a typed path and a review answer with a trust
/// screen (`read` carries the text a typed path held); the model calls answer with the
/// Search page; a choice answers with how its save ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum AppPluginsEvent {
    Listing(Result<Listing, String>),
    Preview {
        read: Option<String>,
        result: Result<Preview, String>,
    },
    Search(Result<SearchView, String>),
    SearchSaved(Result<SearchSaved, String>),
}

impl AppPluginsEvent {
    /// The reply for a request that reached no trusted connection. Input: the job. Output:
    /// its event carrying `trusted_connection_unavailable`.
    pub(in crate::tui) fn unavailable(job: &AppPluginsJob) -> Self {
        failed(job, "trusted_connection_unavailable".into())
    }
}

/// The reply a job gets when it fails before or at the engine. Inputs: the job and the
/// bounded code. Output: the event of the job's kind carrying the code.
fn failed(job: &AppPluginsJob, code: String) -> AppPluginsEvent {
    match job {
        AppPluginsJob::Preview { .. }
        | AppPluginsJob::ReadFile { .. }
        | AppPluginsJob::Review { .. } => AppPluginsEvent::Preview {
            read: None,
            result: Err(code),
        },
        AppPluginsJob::Search
        | AppPluginsJob::Download { .. }
        | AppPluginsJob::CancelDownload
        | AppPluginsJob::DeleteModel => AppPluginsEvent::Search(Err(code)),
        AppPluginsJob::SetSearch { .. } => AppPluginsEvent::SearchSaved(Err(code)),
        AppPluginsJob::List
        | AppPluginsJob::Add { .. }
        | AppPluginsJob::Trust { .. }
        | AppPluginsJob::SetEnabled { .. }
        | AppPluginsJob::Remove { .. }
        | AppPluginsJob::BindAccount { .. }
        | AppPluginsJob::SetKey { .. }
        | AppPluginsJob::Unbind { .. } => AppPluginsEvent::Listing(Err(code)),
    }
}

/// The name a job is logged and refused under, never its text or key. Input: the job.
/// Output: the name.
pub(in crate::tui) fn job_name(job: &AppPluginsJob) -> &'static str {
    match job {
        AppPluginsJob::List => "AppPlugins",
        AppPluginsJob::Preview { .. } => "AppPluginPreview",
        AppPluginsJob::ReadFile { .. } => "AppPluginReadFile",
        AppPluginsJob::Review { .. } => "AppPluginReview",
        AppPluginsJob::Add { .. } => "AppPluginAdd",
        AppPluginsJob::Trust { .. } => "AppPluginTrust",
        AppPluginsJob::SetEnabled { .. } => "AppPluginSetEnabled",
        AppPluginsJob::Remove { .. } => "AppPluginRemove",
        AppPluginsJob::BindAccount { .. } => "AppPluginBindAccount",
        AppPluginsJob::SetKey { .. } => "AppPluginSetKey",
        AppPluginsJob::Unbind { .. } => "AppPluginUnbind",
        AppPluginsJob::Search => "SearchSettings",
        AppPluginsJob::SetSearch { .. } => "SetSearch",
        AppPluginsJob::Download { .. } => "SearchModelDownload",
        AppPluginsJob::CancelDownload => "SearchModelCancel",
        AppPluginsJob::DeleteModel => "SearchModelDelete",
    }
}

/// Reads a manifest file the owner typed the path of, as the desktop reads a chosen one:
/// an absolute path to a regular file, at most `MANIFEST_BYTES_MAX` bytes of UTF-8 text.
/// Input: the path. Output: the text, or `app_plugin_file_unreadable`,
/// `app_plugin_request_bounds` or `app_plugin_file_not_text`.
pub(in crate::tui) fn read_manifest(path: &Path) -> Result<String, String> {
    let unreadable = || "app_plugin_file_unreadable".to_owned();
    if !path.is_absolute() || !std::fs::metadata(path).map_err(|_| unreadable())?.is_file() {
        return Err(unreadable());
    }
    let file = std::fs::File::open(path).map_err(|_| unreadable())?;
    if !file.metadata().map_err(|_| unreadable())?.is_file() {
        return Err(unreadable());
    }
    let mut bytes = Vec::new();
    file.take(MANIFEST_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    if bytes.len() > MANIFEST_BYTES_MAX {
        return Err("app_plugin_request_bounds".into());
    }
    String::from_utf8(bytes).map_err(|_| "app_plugin_file_not_text".into())
}

impl NativeOwner {
    /// Runs one App plugins or Search request through the trusted owner controller.
    /// Inputs: the job and the lost-connection flag. Output: the event; a transport
    /// failure sets the flag so the shell reconnects.
    pub(super) async fn app_plugins(
        &mut self,
        job: AppPluginsJob,
        lost: &mut bool,
    ) -> AppPluginsEvent {
        let (job, read) = match job {
            AppPluginsJob::ReadFile { path } => {
                let text = tokio::task::spawn_blocking(move || read_manifest(&path))
                    .await
                    .unwrap_or_else(|_| Err("app_plugin_file_unreadable".into()));
                match text {
                    Ok(yaml) => (AppPluginsJob::Preview { yaml: yaml.clone() }, Some(yaml)),
                    Err(code) => {
                        return AppPluginsEvent::Preview {
                            read: None,
                            result: Err(code),
                        };
                    }
                }
            }
            other => (other, None),
        };
        let controller = match self.settings_controller() {
            Ok(controller) => controller,
            Err(code) => return failed(&job, code),
        };
        let listing = |result: Result<Listing, crate::engine::EngineError>, lost: &mut bool| {
            AppPluginsEvent::Listing(result.map_err(|error| problem(&error, lost)))
        };
        let search = |result: Result<SearchView, crate::engine::EngineError>, lost: &mut bool| {
            AppPluginsEvent::Search(result.map_err(|error| problem(&error, lost)))
        };
        match job {
            AppPluginsJob::List => listing(controller.app_plugins().await, lost),
            AppPluginsJob::Preview { yaml } => AppPluginsEvent::Preview {
                read,
                result: controller
                    .app_plugin_preview(yaml)
                    .await
                    .map_err(|error| problem(&error, lost)),
            },
            AppPluginsJob::ReadFile { .. } => unreachable!("a typed path is read above"),
            AppPluginsJob::Review { name } => AppPluginsEvent::Preview {
                read: None,
                result: controller
                    .app_plugin_review(name)
                    .await
                    .map_err(|error| problem(&error, lost)),
            },
            AppPluginsJob::Add {
                yaml,
                sha256,
                replace,
            } => listing(controller.app_plugin_add(yaml, sha256, replace).await, lost),
            AppPluginsJob::Trust { name, sha256 } => {
                listing(controller.app_plugin_trust(name, sha256).await, lost)
            }
            AppPluginsJob::SetEnabled { name, enabled } => {
                listing(controller.app_plugin_set_enabled(name, enabled).await, lost)
            }
            AppPluginsJob::Remove { name } => {
                listing(controller.app_plugin_remove(name).await, lost)
            }
            AppPluginsJob::BindAccount {
                name,
                secret,
                account,
            } => listing(
                controller
                    .app_plugin_bind_account(name, secret, account)
                    .await,
                lost,
            ),
            AppPluginsJob::SetKey { name, secret, key } => {
                listing(controller.app_plugin_set_key(name, secret, key).await, lost)
            }
            AppPluginsJob::Unbind { name, secret } => {
                listing(controller.app_plugin_unbind(name, secret).await, lost)
            }
            AppPluginsJob::Search => search(controller.search_settings().await, lost),
            AppPluginsJob::SetSearch {
                choice,
                confirm_host,
                version,
            } => AppPluginsEvent::SearchSaved(
                controller
                    .set_search(choice, confirm_host, version)
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
            AppPluginsJob::Download { confirm_bytes } => {
                search(controller.search_model_download(confirm_bytes).await, lost)
            }
            AppPluginsJob::CancelDownload => search(controller.search_model_cancel().await, lost),
            AppPluginsJob::DeleteModel => search(controller.search_model_delete().await, lost),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_typed_manifest_path_is_read_within_its_bounds() {
        let folder = tempfile::tempdir().unwrap();
        let fits = folder.path().join("fits.yaml");
        std::fs::write(&fits, "schema: 1\n").unwrap();
        assert_eq!(read_manifest(&fits), Ok("schema: 1\n".into()));
        let edge = folder.path().join("edge.yaml");
        std::fs::write(&edge, "#".repeat(MANIFEST_BYTES_MAX)).unwrap();
        assert_eq!(
            read_manifest(&edge).map(|text| text.len()),
            Ok(MANIFEST_BYTES_MAX)
        );
        let long = folder.path().join("long.yaml");
        std::fs::write(&long, "#".repeat(MANIFEST_BYTES_MAX + 1)).unwrap();
        assert_eq!(
            read_manifest(&long),
            Err("app_plugin_request_bounds".into())
        );
        let binary = folder.path().join("binary.yaml");
        std::fs::write(&binary, [0xff_u8, 0xfe, 0x00]).unwrap();
        assert_eq!(
            read_manifest(&binary),
            Err("app_plugin_file_not_text".into())
        );
        let unreadable = Err("app_plugin_file_unreadable".to_owned());
        assert_eq!(read_manifest(folder.path()), unreadable, "a folder");
        assert_eq!(read_manifest(&folder.path().join("gone.yaml")), unreadable);
        assert_eq!(
            read_manifest(Path::new("fits.yaml")),
            unreadable,
            "a relative path"
        );
        let closed = folder.path().join("closed.yaml");
        std::fs::write(&closed, "schema: 1\n").unwrap();
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&closed).is_err() {
            assert_eq!(
                read_manifest(&closed),
                unreadable,
                "a file this user cannot open"
            );
        }
    }

    #[test]
    fn a_job_prints_its_name_never_its_key() {
        let job = AppPluginsJob::SetKey {
            name: "harbor".into(),
            secret: "api_key".into(),
            key: AccountSecret::new(b"sk-test-never-printed".to_vec()).unwrap(),
        };
        assert_eq!(format!("{job:?}"), "AppPluginSetKey");
        assert_eq!(
            AppPluginsEvent::unavailable(&job),
            AppPluginsEvent::Listing(Err("trusted_connection_unavailable".into()))
        );
        assert_eq!(
            AppPluginsEvent::unavailable(&AppPluginsJob::Download {
                confirm_bytes: 125_729_604
            }),
            AppPluginsEvent::Search(Err("trusted_connection_unavailable".into()))
        );
    }
}
