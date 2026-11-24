use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

use super::install_release;
use kernel_processes::update::test_support::{TestResult, at, joined, response, serve};
use kernel_processes::update::{Asset, Manifest, target};

/// The names in a folder with each regular file's bytes, sorted. Input: the folder.
/// Output: the names and bytes, none for a folder, or the listing step that failed, naming
/// its path.
fn listing(folder: &Path) -> TestResult<Vec<(String, Option<Vec<u8>>)>> {
    let mut names = at(folder, std::fs::read_dir(folder))?
        .map(|entry| {
            let entry = at(folder, entry)?;
            let path = entry.path();
            let bytes = if at(&path, entry.file_type())?.is_file() {
                Some(at(&path, std::fs::read(&path))?)
            } else {
                None
            };
            Ok((entry.file_name().to_string_lossy().into_owned(), bytes))
        })
        .collect::<TestResult<Vec<(String, Option<Vec<u8>>)>>>()?;
    names.sort();
    Ok(names)
}

/// A release whose archive matches its checksum but whose signature another key made is
/// refused at the install boundary: the install folder keeps every file byte for byte and
/// no staging folder is left, only the lock file a run always leaves.
#[test]
fn a_release_signed_by_another_key_installs_nothing() -> TestResult {
    let install = tempfile::tempdir()?;
    let engine = install.path().join("crazyagent");
    at(&engine, std::fs::write(&engine, "old engine"))?;
    let runner = install.path().join("crazyagent-runner");
    at(&runner, std::fs::write(&runner, "old runner"))?;
    let before = listing(install.path())?;
    let bytes = b"release archive bytes".to_vec();
    let sha: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let trusted = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let stranger = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let key = STANDARD.encode(trusted.pk.to_box().unwrap().into_string());
    let comment = "timestamp:1761400000\tfile:crazyagent.tar.gz\tversion:9.9.9";
    let signature = minisign::sign(
        Some(&stranger.pk),
        &stranger.sk,
        std::io::Cursor::new(&bytes),
        Some(comment),
        None,
    )
    .unwrap();
    let (url, server) = serve(vec![response(
        "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n",
        &bytes,
    )])?;
    let manifest = Manifest {
        version: "9.9.9".to_owned(),
        platforms: [(
            target().expect("a published platform").to_owned(),
            Asset {
                url,
                sha256: sha,
                signature: STANDARD.encode(signature.into_string()),
            },
        )]
        .into_iter()
        .collect(),
    };
    let refused = install_release(&manifest, install.path(), &key).unwrap_err();
    joined(server)?;
    assert_eq!(
        refused.code, "update_signature_invalid",
        "{}",
        refused.message
    );
    let mut after = listing(install.path())?;
    after.retain(|(name, _)| name != super::UPDATE_LOCK);
    assert_eq!(after, before);
    Ok(())
}
