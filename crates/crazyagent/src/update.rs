use kernel_processes::update::{
    Channel, DOWNLOAD_BYTES_MAX, DOWNLOAD_TIMEOUT, MANIFEST_BYTES_MAX, Manifest, UpdateError,
    data_root, download, fail, inside_app_bundle, io_failure, is_newer, not_published,
    parse_manifest, public_key, read_channel, service_step, swap, target, unpack, verify_file,
    write_channel,
};
use std::env::consts::EXE_SUFFIX;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

const USAGE: &str = "usage: crazyagent update [--check] [--channel stable|dev]";

/// Saves the update channel to `<root>/update.toml` (owner-only on Unix), replacing the
/// file atomically, and creates a missing data folder as the engine does: private to
/// this user (0700). Inputs: the data folder and the channel. Output: Ok, or `update_io`.
pub fn save_channel(root: &Path, channel: Channel) -> Result<(), UpdateError> {
    crate::paths::private_folder(root).map_err(|error| {
        fail(
            "update_io",
            format!("Can't save the update channel: {error}"),
        )
    })?;
    write_channel(root, channel)
}

struct Staging {
    path: PathBuf,
}

impl Staging {
    fn create(install: &Path) -> Result<Self, UpdateError> {
        let path = install.join(format!(".crazyagent-update-{}", std::process::id()));
        let failed = |error| io_failure(&format!("Can't write to {}", install.display()), error);
        if std::fs::symlink_metadata(&path).is_ok() {
            std::fs::remove_dir_all(&path).map_err(failed)?;
        }
        std::fs::create_dir(&path).map_err(failed)?;
        Ok(Self { path })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn install_release(manifest: &Manifest, install: &Path, key: &str) -> Result<String, UpdateError> {
    let platform = target().ok_or_else(|| {
        fail(
            "update_platform_missing",
            "CrazyAgent publishes no release for this computer's platform.",
        )
    })?;
    let asset = manifest.platforms.get(platform).ok_or_else(|| {
        fail(
            "update_platform_missing",
            format!("Release {} has no build for {platform}.", manifest.version),
        )
    })?;
    let staging = Staging::create(install)?;
    let archive = staging.path.join("release-archive");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&archive)
        .map_err(|error| io_failure("Can't save the download", error))?;
    download(&asset.url, &mut file, DOWNLOAD_BYTES_MAX, DOWNLOAD_TIMEOUT)?;
    file.sync_all()
        .map_err(|error| io_failure("Can't save the download", error))?;
    drop(file);
    verify_file(
        &archive,
        &asset.sha256,
        &asset.signature,
        key,
        &manifest.version,
    )?;
    let unpacked = unpack(&archive, &staging.path.join("unpacked"))?;
    let layout = crate::paths::Layout::current().map_err(|_| {
        fail(
            "update_no_data_root",
            "Can't find CrazyAgent's data folder. Set CRAZYAGENT_HOME to an absolute path.",
        )
    })?;
    swap_around_engine(&layout, install, &unpacked, &manifest.version)
}

/// Swaps the unpacked release into the install folder with the engine stopped around it.
/// An installed engine service is stopped and started through `crazyagent service`. An
/// engine that runs without it is stopped here and started again, detached, from the new
/// binary. With no service and no engine running, only the files change. A failure that
/// says nothing changed is only returned before the swap.
/// Inputs: the engine layout, the install folder, the unpacked folder and the version.
/// Output: the sentence for the user, or the failure.
pub fn swap_around_engine(
    layout: &crate::paths::Layout,
    install: &Path,
    unpacked: &Path,
    version: &str,
) -> Result<String, UpdateError> {
    let current = install.join(format!("crazyagent{EXE_SUFFIX}"));
    let stop_failed = |detail: String| {
        fail(
            "update_service_failed",
            format!("Couldn't stop the engine for the update, so nothing changed. {detail}")
                .trim_end()
                .to_owned(),
        )
    };
    let engine =
        crate::service::current_user(kernel_common::Platform::CURRENT, current.clone(), layout)
            .map_err(|error| stop_failed(error.0))?;
    if engine.definition.is_file() {
        let service = |action: &str| {
            service_step(
                Command::new(&current)
                    .arg("service")
                    .arg(action)
                    .env("CRAZYAGENT_HOME", layout.root()),
                action,
            )
        };
        service("stop")?;
        if let Err(error) = swap(install, unpacked, version) {
            return Err(match service("start") {
                Ok(_) => error,
                Err(start) => fail(error.code, format!("{} {}", error.message, start.message)),
            });
        }
        return Ok(match service("start") {
            Ok(_) => format!("Updated crazyagent to {version}. The engine service restarted."),
            Err(start) => format!("Updated crazyagent to {version}. {}", start.message),
        });
    }
    let stopped = crate::service::stop_detached(layout).map_err(|error| stop_failed(error.0))?;
    let swapped = swap(install, unpacked, version);
    if stopped.is_none() {
        return swapped.map(|()| format!("Updated crazyagent to {version}."));
    }
    let restart = || restart_detached(layout, &engine, &current);
    if let Err(error) = swapped {
        return Err(match restart() {
            Ok(()) => error,
            Err(start) => fail(error.code, format!("{} {start}", error.message)),
        });
    }
    Ok(match restart() {
        Ok(()) => format!("Updated crazyagent to {version}. The engine restarted."),
        Err(start) => format!("Updated crazyagent to {version}. {start}"),
    })
}

/// Starts the engine again without the service from the installed `crazyagent`, as a client
/// does when no engine answers. Inputs: the layout, the uninstalled service and the
/// executable. Output: Ok once it answers, or the sentence that says how to start it.
fn restart_detached(
    layout: &crate::paths::Layout,
    service: &kernel_processes::service::Service,
    executable: &Path,
) -> Result<(), String> {
    let again = "The engine didn't start again. Open CrazyAgent to start it: crazyagent tui";
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| again.to_owned())?;
    runtime
        .block_on(crate::service::ensure_running(
            layout,
            service,
            executable,
            std::env::var_os("PATH").as_deref(),
            kernel_processes::service::READY_TIMEOUT,
        ))
        .map(|_| ())
        .map_err(|error| format!("{again} ({})", error.0))
}

fn fetch_manifest(channel: Channel, url: &str) -> Result<Manifest, UpdateError> {
    let mut bytes = Vec::new();
    download(url, &mut bytes, MANIFEST_BYTES_MAX, DOWNLOAD_TIMEOUT).map_err(|error| {
        if error.code == "update_download_missing" {
            not_published(channel)
        } else {
            error
        }
    })?;
    parse_manifest(&bytes)
}

#[derive(Debug)]
struct Options {
    check: bool,
    channel: Option<Channel>,
}

fn parse_arguments(args: &[OsString]) -> Result<Options, UpdateError> {
    let usage = || fail("update_usage", USAGE);
    let mut options = Options {
        check: false,
        channel: None,
    };
    let mut words = args.iter().map(|arg| arg.to_str().ok_or_else(usage));
    while let Some(word) = words.next() {
        match word? {
            "--check" if !options.check => options.check = true,
            "--channel" if options.channel.is_none() => {
                let word = words.next().ok_or_else(usage)??;
                options.channel = Some(Channel::parse(word).ok_or_else(usage)?);
            }
            _ => return Err(usage()),
        }
    }
    Ok(options)
}

/// `crazyagent update [--check] [--channel stable|dev]`: reads the channel (`--channel` also
/// saves it), fetches that channel's `cli-latest.json`, and with `--check` only reports. Else
/// it downloads this platform's archive next to the executable, verifies its SHA-256 and
/// signature, stops the engine service, swaps the executables and starts the service again.
/// An executable inside the desktop app is refused. Input: the arguments after `update`.
/// Output: one line for the owner, or the failure; a failure changes no installed file.
pub fn run(args: &[OsString]) -> Result<String, UpdateError> {
    let options = parse_arguments(args)?;
    let root = data_root().ok_or_else(|| {
        fail(
            "update_no_data_root",
            "Can't find CrazyAgent's data folder. Set CRAZYAGENT_HOME to an absolute path.",
        )
    })?;
    let channel = match options.channel {
        Some(channel) => {
            save_channel(&root, channel)?;
            channel
        }
        None => read_channel(&root)?,
    };
    let manifest = fetch_manifest(channel, channel.cli_manifest_url())?;
    let current = env!("CARGO_PKG_VERSION");
    if !is_newer(&manifest.version, current)? {
        return Ok(format!(
            "crazyagent {current} is up to date on the {} channel.",
            channel.as_str()
        ));
    }
    if options.check {
        return Ok(format!(
            "crazyagent {} is available on the {} channel. This is {current}. Run crazyagent update to install it.",
            manifest.version,
            channel.as_str()
        ));
    }
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| io_failure("Can't find this crazyagent", error))?;
    if inside_app_bundle(&executable) {
        return Err(fail(
            "update_inside_app",
            "This crazyagent belongs to the CrazyAgent app. Update the app instead: open CrazyAgent, then Updates in the menu under your name.",
        ));
    }
    let install = executable
        .parent()
        .ok_or_else(|| fail("update_io", "Can't find the folder holding crazyagent."))?;
    install_release(&manifest, install, &public_key()?)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod lock_tests;
