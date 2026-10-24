use super::{BridgeError, NativeBridge, failure};
use crazyagent::project_setup::DOCUMENT_EXTENSIONS;
use std::path::{Component, Path, PathBuf};

const LINK_BYTES_MAX: usize = 2048;
const PATH_BYTES_MAX: usize = 4096;

/// Checks a link before the system browser opens it: one http or https URL with a host and
/// no user name or password, at most 2048 bytes, no spaces or control characters.
/// Input: the link text. Output: the parsed link, or `link_not_allowed`.
pub fn link_allowed(text: &str) -> Result<String, BridgeError> {
    let refused = || failure("link_not_allowed");
    let link = text.trim_matches(' ');
    if link.is_empty()
        || link.len() > LINK_BYTES_MAX
        || link.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(refused());
    }
    let url = tauri::Url::parse(link).map_err(|_| refused())?;
    let web = matches!(url.scheme(), "https" | "http");
    let host = url.host_str().is_some_and(|host| !host.is_empty());
    if !web || !host || !url.username().is_empty() || url.password().is_some() {
        return Err(refused());
    }
    let parsed = url.to_string();
    assert!(parsed.starts_with("http"));
    Ok(parsed)
}

/// Resolves a project's stored folder. Input: the folder the engine recorded.
/// Output: its canonical path when it is an existing absolute folder, or `project_folder_missing`.
pub fn project_folder(stored: &str) -> Result<PathBuf, BridgeError> {
    let missing = || failure("project_folder_missing");
    let path = Path::new(stored);
    if !path.is_absolute() || stored.len() > PATH_BYTES_MAX {
        return Err(missing());
    }
    let canonical = path.canonicalize().map_err(|_| missing())?;
    if !canonical.is_dir() {
        return Err(missing());
    }
    Ok(canonical)
}

/// Resolves one context file to open with its default app: a relative path of plain parts,
/// a document type the project accepts, not a link, and a regular file still inside the
/// project folder. Inputs: the canonical project folder and the relative path.
/// Output: the file's canonical path, or `context_item_not_openable`.
pub fn context_file(folder: &Path, relative: &str) -> Result<PathBuf, BridgeError> {
    assert!(folder.is_absolute());
    let refused = || failure("context_item_not_openable");
    let path = Path::new(relative);
    if relative.is_empty()
        || relative.len() > PATH_BYTES_MAX
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(refused());
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    if !extension.is_some_and(|value| DOCUMENT_EXTENSIONS.contains(&value.as_str())) {
        return Err(refused());
    }
    let joined = folder.join(path);
    let metadata = std::fs::symlink_metadata(&joined).map_err(|_| refused())?;
    if !metadata.file_type().is_file() {
        return Err(refused());
    }
    let canonical = joined.canonicalize().map_err(|_| refused())?;
    if !canonical.starts_with(folder) || !canonical.is_file() {
        return Err(refused());
    }
    Ok(canonical)
}

async fn launch(
    job: impl FnOnce() -> Result<(), &'static str> + Send + 'static,
) -> Result<(), BridgeError> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|_| failure("open_failed"))?
        .map_err(failure)
}

/// Opens one checked web link in the owner's default browser. Input: the link text.
pub async fn open_link(text: &str) -> Result<(), BridgeError> {
    let link = link_allowed(text)?;
    launch(move || crazyagent::platform::open(std::ffi::OsStr::new(&link))).await
}

impl NativeBridge {
    async fn stored_folder(&mut self, id: &str) -> Result<PathBuf, BridgeError> {
        let setup = self
            .project_setup(id)
            .await?
            .ok_or_else(|| failure("project_folder_missing"))?;
        project_folder(&setup.folder)
    }

    /// Shows a project's folder in the system file manager; nothing in it is opened or run.
    /// Input: the project id.
    pub async fn reveal_project_folder(&mut self, id: &str) -> Result<(), BridgeError> {
        let folder = self.stored_folder(id).await?;
        launch(move || crazyagent::platform::reveal(&folder)).await
    }

    /// Opens one document or research file from a project folder with its default app.
    /// Inputs: the project id and the file's path inside the folder.
    pub async fn open_context_item(&mut self, id: &str, path: &str) -> Result<(), BridgeError> {
        let folder = self.stored_folder(id).await?;
        let file = context_file(&folder, path)?;
        launch(move || crazyagent::platform::open(file.as_os_str())).await
    }
}
