use super::git::Repo;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The commit time of every signed commit these rows make, as git prints `%ct`.
const SIGNED_AT: &str = "1700000000";
/// The subject of every signed commit these rows make.
const SUBJECT: &str = "Signed subject";
/// The signature programs a signed repository names, each writing its marker when run.
const PROGRAMS: [(&str, &str); 4] = [
    ("gpg.program", "gpg"),
    ("gpg.openpgp.program", "openpgp"),
    ("gpg.x509.program", "x509"),
    ("gpg.ssh.program", "ssh"),
];

/// The armor of one signed commit, which picks the program git verifies it with.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Armor {
    Pgp,
    Ssh,
    X509,
}

impl Armor {
    /// Every armor git tells apart.
    pub(crate) const ALL: [Self; 3] = [Self::Pgp, Self::Ssh, Self::X509];

    /// The first and last lines of a signature in this armor.
    fn lines(self) -> (&'static str, &'static str) {
        match self {
            Self::Pgp => (
                "-----BEGIN PGP SIGNATURE-----",
                "-----END PGP SIGNATURE-----",
            ),
            Self::Ssh => (
                "-----BEGIN SSH SIGNATURE-----",
                "-----END SSH SIGNATURE-----",
            ),
            Self::X509 => (
                "-----BEGIN SIGNED MESSAGE-----",
                "-----END SIGNED MESSAGE-----",
            ),
        }
    }
}

/// An owner repository whose one commit, its HEAD, carries a signature, and whose own
/// config asks every log for signatures and names a marker-writing program for each
/// signature kind, with the folder those markers land in.
pub(crate) struct Signed {
    pub(crate) root: PathBuf,
    pub(crate) base: PathBuf,
    markers: PathBuf,
    _temp: tempfile::TempDir,
}

impl Signed {
    /// The signature programs that ran, by marker name, sorted.
    pub(crate) fn ran(&self) -> Vec<String> {
        let mut ran: Vec<String> = fs::read_dir(&self.markers)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        ran.sort();
        ran
    }
}

/// Runs the test's own git in `repo` with a private home and no system config. Inputs:
/// the repository, the home and the arguments. Output: git's standard output.
fn git(repo: &Path, home: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .args(args)
            .current_dir(repo)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// Makes a [`Signed`] repository whose HEAD is signed in `armor`. Input: the armor.
/// Output: the repository.
pub(crate) fn signed(armor: Armor) -> Signed {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let root = base.join("owner");
    let home = base.join("home");
    let markers = base.join("markers");
    for folder in [&root, &home, &markers] {
        fs::create_dir_all(folder).unwrap();
    }
    git(&root, &home, &["init", "-q", "--template="]);
    let tree = git(
        &root,
        &home,
        &["hash-object", "-t", "tree", "-w", "/dev/null"],
    );
    let (begin, end) = armor.lines();
    let person = format!("Owner <owner@example.invalid> {SIGNED_AT} +0000");
    let text = format!(
        "tree {}\nauthor {person}\ncommitter {person}\ngpgsig {begin}\n \n U0lHTkFUVVJF\n {end}\n\n{SUBJECT}\n",
        tree.trim()
    );
    let file = base.join("commit.txt");
    fs::write(&file, text).unwrap();
    let commit = git(
        &root,
        &home,
        &["hash-object", "-t", "commit", "-w", file.to_str().unwrap()],
    );
    git(&root, &home, &["update-ref", "HEAD", commit.trim()]);
    let signers = base.join("allowed_signers");
    fs::write(&signers, "").unwrap();
    git(&root, &home, &["config", "log.showSignature", "true"]);
    git(
        &root,
        &home,
        &[
            "config",
            "gpg.ssh.allowedSignersFile",
            signers.to_str().unwrap(),
        ],
    );
    for (key, name) in PROGRAMS {
        let program = base.join(format!("{name}.sh"));
        let script = format!(
            "#!/bin/sh\ntouch '{}'\nexit 1\n",
            markers.join(name).display()
        );
        fs::write(&program, script).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        git(&root, &home, &["config", key, program.to_str().unwrap()]);
    }
    Signed {
        root,
        base,
        markers,
        _temp: temp,
    }
}

/// The roadmap status Refresh reads the subjects of a signed owner branch whose
/// repository asks every log for signatures, and runs none of the repository's signature
/// programs as the owner (RV-DS241-39).
#[test]
fn roadmap_evidence_reads_a_signed_branch_and_runs_no_signature_program() {
    let signed = signed(Armor::Pgp);
    let subjects = super::evidence::subjects(&signed.root, 10).unwrap();
    assert_eq!(subjects, vec![SUBJECT.to_owned()]);
    assert_eq!(signed.ran(), Vec::<String>::new());
}

/// Engine git shows a signed owner tip, as a clone's freshness check and a landing's
/// subject read do, and runs none of the repository's signature programs (RV-DS241-39).
#[test]
fn engine_git_shows_a_signed_owner_tip_and_runs_no_signature_program() {
    let signed = signed(Armor::Pgp);
    let owner = Repo::open(&signed.root, &signed.root.join(".git"), None).unwrap();
    let shown = owner
        .capture(&["show", "-s", "--format=%ct", "HEAD"], 64)
        .unwrap();
    assert_eq!(String::from_utf8(shown).unwrap(), format!("{SIGNED_AT}\n"));
    assert_eq!(signed.ran(), Vec::<String>::new());
}

/// The engine's setup git, asked outright for the signature of a commit in any armor,
/// runs none of the repository's signature programs (RV-DS241-39).
#[test]
fn engine_setup_git_runs_no_repository_signature_program_in_any_armor() {
    for armor in Armor::ALL {
        let signed = signed(armor);
        let tmp = signed.base.join("tmp");
        fs::create_dir_all(&tmp).unwrap();
        crate::research::runner::engine_git(
            &signed.root,
            &tmp,
            &["log", "--show-signature", "-1", "--format=%s"],
        )
        .unwrap();
        assert_eq!(signed.ran(), Vec::<String>::new(), "{armor:?}");
    }
}
