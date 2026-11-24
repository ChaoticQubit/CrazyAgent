#![cfg(target_os = "macos")]

use base64::Engine;
use crazyagent::ide::sandbox::{Grants, profile_with};
use kernel_sandbox::net::SandboxNetwork;
use kernel_sandbox::policy::{AgentFilesRule, IdeAgentFiles};
use kernel_sandbox::toolchains::ToolHomes;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const ROOT: &str = "/private/tmp/ca-e12-golden";
const OWNER_TOKEN: &str = "{OWNER_HOME}";

/// Makes the fixed layout fresh: refuses a linked root, removes what an earlier run left,
/// then creates the data, build, server home, project, tmp and empty owner folders, each
/// 0700. Input: none. Output: the root.
fn fresh_root() -> PathBuf {
    let root = Path::new(ROOT);
    match fs::symlink_metadata(root) {
        Ok(found) => {
            assert!(
                found.is_dir() && !found.file_type().is_symlink(),
                "{ROOT} is not a plain folder"
            );
            fs::remove_dir_all(root).unwrap();
        }
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
    for path in [
        "data/ide/p1/server",
        "data/ide/builds/v1",
        "project",
        "tmp",
        "owner",
    ] {
        let folder = root.join(path);
        fs::create_dir_all(&folder).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for folder in [root.to_path_buf(), root.join("data"), root.join("data/ide")] {
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(fs::canonicalize(root).unwrap(), root);
    root.to_path_buf()
}

/// Undoes the case folding a Seatbelt regex writes, so a folded path reads as plain
/// lowercase text: `[xX]` becomes `x`, and a group whose choices all spell one ASCII word
/// in any case or one character becomes that word. Input: the profile. Output: the text.
fn unfolded(text: &str) -> String {
    let mut out = text.to_owned();
    loop {
        let before = out.clone();
        let chars: Vec<char> = out.chars().collect();
        let mut next = String::with_capacity(out.len());
        let mut index = 0;
        while index < chars.len() {
            if chars[index] == '['
                && index + 3 < chars.len()
                && chars[index + 3] == ']'
                && chars[index + 1].is_ascii_alphabetic()
                && chars[index + 1].eq_ignore_ascii_case(&chars[index + 2])
            {
                next.push(chars[index + 1].to_ascii_lowercase());
                index += 4;
                continue;
            }
            if chars[index] == '('
                && let Some(length) = chars[index + 1..].iter().position(|c| *c == ')')
            {
                let inner: String = chars[index + 1..index + 1 + length].iter().collect();
                let choices: Vec<&str> = inner.split('|').collect();
                let word = choices[0].to_ascii_lowercase();
                let plain = !inner.contains('(')
                    && choices.len() > 1
                    && !word.is_empty()
                    && word.chars().all(|c| c.is_ascii_alphabetic())
                    && choices[1..].iter().all(|choice| {
                        choice.to_ascii_lowercase() == word || choice.chars().count() == 1
                    });
                if plain {
                    next.push_str(&word);
                    index += length + 2;
                    continue;
                }
            }
            next.push(chars[index]);
            index += 1;
        }
        out = next;
        if out == before {
            return out.to_lowercase();
        }
    }
}

/// Writes the editor profile this commit builds over the fixed root and empty tool homes to
/// the file `E12_GOLDEN_OUT` names: the owner's home, then each case in base64 with its
/// sha256. Each case is built twice on a fresh root and must match, and no form of the
/// owner's home or of another temp root may be left once its raw home is a token. Input:
/// the variable; without it the row does nothing. Output: the file.
#[test]
fn the_base_editor_profile_bytes() {
    let Some(out) = std::env::var_os("E12_GOLDEN_OUT") else {
        return;
    };
    let owner = kernel_sandbox::paths::owner_home().unwrap();
    let owner_text = owner.to_str().unwrap().to_owned();
    let owner_name = owner.file_name().unwrap().to_str().unwrap().to_lowercase();
    let read_only = AgentFilesRule {
        choice: IdeAgentFiles::ReadOnly,
        older_agents: true,
    };
    let cases = [
        (
            "closed",
            SandboxNetwork::Only(Vec::new()),
            Vec::new(),
            AgentFilesRule::default(),
        ),
        (
            "open_kept",
            SandboxNetwork::Open,
            vec![11_434],
            AgentFilesRule::default(),
        ),
        (
            "closed_read_only",
            SandboxNetwork::Only(Vec::new()),
            Vec::new(),
            read_only,
        ),
    ];
    let mut text = format!("root={ROOT}\nowner_home={owner_text}\n");
    for (name, network, kept, rule) in cases {
        let build = || {
            let root = fresh_root();
            let (data, home) = (root.join("data"), root.join("data/ide/p1/server"));
            let (build, project, tmp) = (
                root.join("data/ide/builds/v1"),
                root.join("project"),
                root.join("tmp"),
            );
            let grants = Grants {
                data: &data,
                root: &project,
                build: &build,
                home: &home,
                tmp: &tmp,
                writable: &[],
                read_only: &[],
                readable: &[],
                locked: &[],
            };
            let homes = ToolHomes::at(&root.join("owner"));
            profile_with(&homes, &grants, 45_731, network.clone(), &kept, rule).unwrap()
        };
        let bytes = build();
        assert_eq!(bytes, build(), "{name} is not reproducible on one machine");
        let plain = unfolded(&bytes.replace(&owner_text, OWNER_TOKEN));
        assert!(plain.contains("/private/tmp/ca-e12-golden/"), "{name}");
        assert!(
            !plain.contains(&owner_name),
            "{name} keeps a form of the owner's home"
        );
        for other in ["/var/folders/", "ca-e12g-", "/applications/xcode"] {
            assert!(!plain.contains(other), "{name} holds {other}");
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes.as_bytes());
        let sum = Sha256::digest(bytes.as_bytes());
        let hex: String = sum.iter().map(|byte| format!("{byte:02x}")).collect();
        text.push_str(&format!("{name}={encoded}\n{name}_sha256={hex}\n"));
    }
    fs::remove_dir_all(ROOT).unwrap();
    fs::write(out, text).unwrap();
}
