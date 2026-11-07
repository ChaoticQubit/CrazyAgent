use crate::engine::EngineError;
use std::io::{Read, Write};
use std::path::Path;

/// The workbench bundle the browser loads, relative to an extracted build's root.
pub const BUNDLE: &str = "out/vs/code/browser/workbench/workbench.js";

/// Names the set of [`EDITS`] a build was installed with, so a build installed under another
/// revision is fetched again. Change it whenever [`EDITS`] change.
pub const REVISION: &str = "h3";

const BUNDLE_BYTES_MAX: u64 = 64 * 1024 * 1024;
const WINDOW: usize = 32;

/// One edit to the bundle: the first `from` starting within [`WINDOW`] bytes after the
/// bundle's only `anchor` becomes `to`.
struct Edit {
    anchor: &'static str,
    from: &'static str,
    to: &'static str,
}

/// The removed parts no setting or extension can hide in the pinned build: the palette
/// entries of the Remote menu, remote extension install, the Accounts commands, account
/// policy sync and diagnostics, inline suggestion snoozing, dynamic authentication providers
/// and Copilot; the Remote menu's shortcut; the Remote indicator (`status.host`); the Accounts
/// entry, read from a stored choice; and the Settings Chat section. Remote connections stay.
const EDITS: [Edit; 18] = [
    Edit {
        anchor: "\"Show Remote Menu\")",
        from: "f1:!0,keybinding:{weight:200,primary:2605}",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Install Remote Development Extensions\")",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Sync Account Policy\")",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Snooze Inline Suggestions\")",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Manage Accounts\")",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Manage Trusted Extensions For Account\")",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Manage Extension Account Preferences...\")",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Manage Trusted MCP Servers For Account\")",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "}renderRemoteStatusIndicator(",
        from: "){",
        to: "){return;",
    },
    Edit {
        anchor: "\"Cancel Snooze Inline Suggestions\")",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Remove Dynamic Authentication Providers\"),category:",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Policy Diagnostics\")",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Manage Copilot Settings\")",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Show Extensions using Copilot\")",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Upgrade to GitHub Copilot Pro\")",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Manage GitHub Copilot Budget\")",
        from: "f1:!0",
        to: "f1:!1",
    },
    Edit {
        anchor: "\"Manage\" can not be hidden')}})]}",
        from: "{return ",
        to: "{return!1&&",
    },
    Edit {
        anchor: "{id:\"chat\",label:",
        from: "children:[",
        to: "hide:!0,children:[",
    },
];

fn failed(_: impl std::fmt::Display) -> EngineError {
    EngineError("ide_build_patch_failed".to_owned())
}

/// Hides the removed parts in an extracted build by rewriting its workbench bundle with
/// [`EDITS`]. The bundle must be a regular file within [`BUNDLE_BYTES_MAX`] where every edit
/// applies, else it is left as it was. The new text goes to a file created fresh beside it,
/// after any leftover entry there is removed, so a link planted at that name is never
/// followed. Input: the build root. Output: Ok, or `ide_build_patch_failed`.
pub fn apply(root: &Path) -> Result<(), EngineError> {
    let path = root.join(BUNDLE);
    let metadata = std::fs::symlink_metadata(&path).map_err(failed)?;
    if !metadata.is_file() || metadata.len() > BUNDLE_BYTES_MAX {
        return Err(failed("bundle"));
    }
    let mut text = String::new();
    std::fs::File::open(&path)
        .map_err(failed)?
        .take(BUNDLE_BYTES_MAX + 1)
        .read_to_string(&mut text)
        .map_err(failed)?;
    if text.len() as u64 > BUNDLE_BYTES_MAX {
        return Err(failed("bundle"));
    }
    let hidden = hide(&text).ok_or_else(|| failed("edit"))?;
    let staged = path.with_extension("js.hiding");
    match std::fs::remove_file(&staged) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(failed(error)),
    }
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .and_then(|mut file| {
            file.write_all(hidden.as_bytes())
                .and_then(|()| file.sync_all())
        })
        .and_then(|()| std::fs::rename(&staged, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    written.map_err(failed)
}

/// Applies every one of [`EDITS`] in order. Input: the bundle text. Output: the new text,
/// or none when an anchor is missing or repeated, or its `from` does not start within
/// [`WINDOW`] bytes after it.
pub(crate) fn hide(bundle: &str) -> Option<String> {
    let mut text = bundle.to_owned();
    for edit in &EDITS {
        let mut found = text.match_indices(edit.anchor);
        let (at, _) = found.next()?;
        if found.next().is_some() {
            return None;
        }
        let start = at + edit.anchor.len();
        let end = text.len().min(start + WINDOW + edit.from.len());
        let offset = text.as_bytes()[start..end]
            .windows(edit.from.len())
            .position(|window| window == edit.from.as_bytes())?;
        let place = start + offset;
        text.replace_range(place..place + edit.from.len(), edit.to);
    }
    let removed: usize = EDITS.iter().map(|edit| edit.from.len()).sum();
    let added: usize = EDITS.iter().map(|edit| edit.to.len()).sum();
    assert_eq!(text.len() + removed, bundle.len() + added);
    Some(text)
}

#[cfg(test)]
pub(crate) mod tests;
