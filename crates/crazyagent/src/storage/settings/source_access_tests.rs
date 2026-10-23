use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

const GLOBAL: &str = "timeout_ms = 1000\n";

struct Fixture {
    sources: SettingsSources,
    config: EffectiveConfig,
    global: PathBuf,
    project: PathBuf,
    root: tempfile::TempDir,
}

fn private_directory(path: &Path) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

impl Fixture {
    fn new(project_present: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-settings-source-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global_parent = root.path().join("global");
        let project_parent = root.path().join("project");
        private_directory(&global_parent);
        private_directory(&project_parent);
        let global = global_parent.join("settings.toml");
        let project = project_parent.join("settings.toml");
        private_file(&global, GLOBAL.as_bytes());
        if project_present {
            private_file(&project, b"");
        }
        let sources = SettingsSources::open(
            &global_parent.join(".").join("settings.toml"),
            &project_parent.join(".").join("settings.toml"),
        )
        .unwrap();
        let config = sources.config().unwrap();
        Self {
            sources,
            config,
            global,
            project,
            root,
        }
    }

    fn retained_hash(&self) -> String {
        verified_source_hash(&self.config, Some(&self.sources)).unwrap()
    }

    fn fresh_hash(&self) -> String {
        verified_source_hash_at(&self.config, &self.global, &self.project).unwrap()
    }
}

#[test]
fn settings_source_access_retained_paths_preserve_order_and_missing_project() {
    let fixture = Fixture::new(false);
    let (global, project) = fixture.sources.retained_paths();
    assert_eq!(global, fixture.global);
    assert_eq!(project, fixture.project);
    assert!(global.is_absolute());
    assert!(project.is_absolute());
    assert!(!project.exists());
    let before = fixture.retained_hash();
    assert_eq!(
        verified_source_hash_at(&fixture.config, &global, &project).unwrap(),
        before
    );
    assert!(!project.exists());
    private_file(&project, b"");
    assert_eq!(fixture.sources.config().unwrap().id, fixture.config.id);
    assert_ne!(fixture.fresh_hash(), before);
    assert_eq!(fixture.fresh_hash(), fixture.retained_hash());
    assert_eq!(fixture.sources.retained_paths(), (global, project));
}

#[test]
fn settings_source_access_hash_matches_live_writer_without_second_lease() {
    let fixture = Fixture::new(true);
    let expected = fixture.retained_hash();
    let lease_identity = identity(&fixture.sources.lease.metadata().unwrap());
    let lease_state = fixture
        .sources
        .global
        .read_name(&fixture.sources.lease_name)
        .unwrap();
    for _ in 0..3 {
        assert_eq!(fixture.fresh_hash(), expected);
        assert_eq!(fixture.sources.config().unwrap().id, fixture.config.id);
        assert_eq!(
            identity(&fixture.sources.lease.metadata().unwrap()),
            lease_identity
        );
        assert_eq!(
            fixture
                .sources
                .global
                .read_name(&fixture.sources.lease_name)
                .unwrap(),
            lease_state
        );
    }
    assert_ne!(expected, fixture.config.id.as_str());
}

#[test]
fn settings_source_access_content_and_file_identity_are_not_config_id() {
    for replace_inode in [false, true] {
        let fixture = Fixture::new(true);
        let before = fixture.retained_hash();
        let old_identity = identity(&std::fs::metadata(&fixture.global).unwrap());
        if replace_inode {
            let replacement = fixture.global.parent().unwrap().join("replacement.toml");
            private_file(&replacement, GLOBAL.as_bytes());
            assert_ne!(
                identity(&std::fs::metadata(&replacement).unwrap()),
                old_identity
            );
            std::fs::rename(replacement, &fixture.global).unwrap();
        } else {
            private_file(&fixture.global, format!("{GLOBAL}\n").as_bytes());
            assert_eq!(
                identity(&std::fs::metadata(&fixture.global).unwrap()),
                old_identity
            );
        }
        assert_eq!(fixture.sources.config().unwrap().id, fixture.config.id);
        let current = fixture.fresh_hash();
        assert_ne!(current, before);
        assert_eq!(current, fixture.retained_hash());
    }
}

#[test]
fn settings_source_access_parent_replacement_is_measured_and_retained_parent_rejected() {
    let fixture = Fixture::new(true);
    let before = fixture.retained_hash();
    let paths = fixture.sources.retained_paths();
    let parent = fixture.global.parent().unwrap();
    std::fs::rename(parent, fixture.root.path().join("original-global")).unwrap();
    private_directory(parent);
    private_file(&fixture.global, GLOBAL.as_bytes());
    assert_eq!(fixture.sources.retained_paths(), paths);
    let resolved = crate::config::resolve_config(&fixture.global, &fixture.project).unwrap();
    assert_eq!(resolved.id, fixture.config.id);
    assert_ne!(fixture.fresh_hash(), before);
    assert_eq!(
        verified_source_hash(&fixture.config, Some(&fixture.sources))
            .unwrap_err()
            .0,
        "settings_parent_replaced"
    );
}

#[test]
fn settings_source_access_stale_configuration_is_rejected() {
    let fixture = Fixture::new(true);
    let before = fixture.retained_hash();
    private_file(&fixture.global, b"timeout_ms = 2000\n");
    let current = fixture.sources.config().unwrap();
    assert_ne!(current.id, fixture.config.id);
    assert_eq!(
        verified_source_hash_at(&fixture.config, &fixture.global, &fixture.project)
            .unwrap_err()
            .0,
        "settings_source_conflict"
    );
    assert_eq!(
        verified_source_hash(&fixture.config, Some(&fixture.sources))
            .unwrap_err()
            .0,
        "settings_source_conflict"
    );
    let actual = verified_source_hash_at(&current, &fixture.global, &fixture.project).unwrap();
    assert_eq!(actual, verified_source_hash(&current, Some(&fixture.sources)).unwrap());
    assert_ne!(actual, before);
}

#[test]
fn settings_source_access_unsafe_files_and_parents_are_rejected() {
    for project in [false, true] {
        for mutation in ["symlink", "hardlink", "file_mode", "parent_mode", "size", "utf8"] {
            let fixture = Fixture::new(true);
            let path = if project {
                &fixture.project
            } else {
                &fixture.global
            };
            let expected = match mutation {
                "symlink" => {
                    let target = path.parent().unwrap().join("original.toml");
                    std::fs::rename(path, &target).unwrap();
                    symlink(target, path).unwrap();
                    None
                }
                "hardlink" => {
                    std::fs::hard_link(path, path.parent().unwrap().join("alias.toml")).unwrap();
                    Some("settings_metadata_invalid")
                }
                "file_mode" => {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
                    Some("settings_metadata_invalid")
                }
                "parent_mode" => {
                    std::fs::set_permissions(
                        path.parent().unwrap(),
                        std::fs::Permissions::from_mode(0o755),
                    )
                    .unwrap();
                    Some("settings_metadata_invalid")
                }
                "size" => {
                    private_file(path, &vec![b' '; crate::config::CONFIG_BYTES_MAX + 1]);
                    Some("settings_file_bounds")
                }
                "utf8" => {
                    private_file(path, &[0xff]);
                    None
                }
                _ => unreachable!(),
            };
            let error = verified_source_hash_at(&fixture.config, &fixture.global, &fixture.project)
                .unwrap_err();
            if let Some(expected) = expected {
                assert_eq!(error.0, expected, "project={project}, mutation={mutation}");
            }
        }
    }
}
