use super::pack::PackManifest;
use super::{Dependency, DependencyState, DependencyStatus, PackEntry, PackState};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a background version check stands before a status read checks again, unless
/// the probes' mark moved first ([`check_due`]).
pub const RECHECK_AFTER: Duration = Duration::from_secs(30);

/// What one engine's setup run is doing now: whether it runs, whether it installs the
/// graphify command (only while the code graph entry is ticked or on), the dependencies it
/// touched with their states, the pack entries still installing or failed with a reason,
/// and the plain reason each entry's last hooks seeding was refused. It also keeps the last
/// background version check, so a status read never waits, with the permission fix target
/// that check found for each refused tool, and why the owner's last fix of a tool did not
/// help (DEC-162), kept apart so the refused row and its command stay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    pub running: bool,
    pub graph_tool: bool,
    pub dependencies: BTreeMap<Dependency, DependencyState>,
    pub pack: BTreeMap<String, PackState>,
    pub hooks: BTreeMap<String, String>,
    pub checking: bool,
    pub checked: Option<Checked>,
    pub fixes: BTreeMap<Dependency, crate::tools::permission_fix::Target>,
    pub fix_failed: BTreeMap<Dependency, String>,
}

/// What the last background version check found for each dependency, when it ended, and
/// the folder probes' mark it read just before building its places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub at: Instant,
    pub found: Vec<(Dependency, crate::setup::Lookup)>,
    pub probe: crate::tools::probe::Mark,
}

/// Whether a status read starts a new version check: none ended yet, the last one is
/// [`RECHECK_AFTER`] old, or the probes' mark moved since it was read, so a tool the login
/// shell's PATH holds shows on the next read. Inputs: the last check, the mark now and now.
/// Output: the answer.
pub fn check_due(checked: Option<&Checked>, mark: crate::tools::probe::Mark, now: Instant) -> bool {
    checked.is_none_or(|checked| {
        checked.probe != mark || now.saturating_duration_since(checked.at) >= RECHECK_AFTER
    })
}

static PROGRESS: Mutex<BTreeMap<PathBuf, Progress>> = Mutex::new(BTreeMap::new());

fn key(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

/// Reads or changes the setup progress of the engine whose root folder is given.
/// Inputs: the engine root and the change. Output: what the change returns.
pub fn with<T>(root: &Path, change: impl FnOnce(&mut Progress) -> T) -> T {
    let mut all = PROGRESS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    change(all.entry(key(root)).or_default())
}

/// Returns a copy of one engine's setup progress. Input: the engine root.
pub fn snapshot(root: &Path) -> Progress {
    with(root, |progress| progress.clone())
}

/// Lists each dependency for the setup screen: the state a run gave it, else what the
/// last version check found, else checking while no check has finished yet. A tool the
/// check did not find reads checking while the probes' first run had not ended, since the
/// login shell's PATH may still hold it. Input: the progress. Output: one row per
/// dependency in install order.
pub fn dependency_rows(progress: &Progress) -> Vec<DependencyStatus> {
    let found = progress
        .checked
        .as_ref()
        .map(|checked| checked.found.as_slice());
    let probing = progress
        .checked
        .as_ref()
        .is_some_and(|checked| checked.probe.first_run_pending);
    let rows: Vec<DependencyStatus> = Dependency::ALL
        .iter()
        .map(|dep| {
            let seen =
                found.map(|found| found.iter().find(|(name, _)| name == dep).map(|(_, f)| f));
            let state = match (progress.dependencies.get(dep), seen) {
                (Some(step), _) => step.clone(),
                (None, None) => DependencyState::Checking,
                (None, Some(Some(Ok(Ok(located))))) if located.bundled => {
                    DependencyState::Bundled {
                        version: located.version.clone(),
                    }
                }
                (
                    None,
                    Some(Some(Ok(Ok(super::Located {
                        version,
                        note: Some(note),
                        ..
                    })))),
                ) => DependencyState::System {
                    version: version.clone(),
                    note: note.line().to_owned(),
                },
                (None, Some(Some(Ok(Ok(located))))) => DependencyState::Found {
                    version: located.version.clone(),
                },
                (None, Some(Some(Err(code)))) => DependencyState::refused(code.0),
                (None, Some(Some(Ok(Err(_))))) if probing => DependencyState::Checking,
                (None, Some(Some(Ok(Err(missing))))) => DependencyState::Missing {
                    line: missing.line(),
                },
                (None, Some(None)) => DependencyState::Missing {
                    line: dep.not_found().line(),
                },
            };
            DependencyStatus {
                name: *dep,
                label: dep.label().to_owned(),
                state,
            }
        })
        .collect();
    assert_eq!(rows.len(), Dependency::ALL.len());
    rows
}

/// Lists each default-pack entry for the setup screen: installing while a run works on
/// it, on or off once installed, the failure a run met, else available, and for an
/// installed entry why its hooks were refused. Inputs: the manifest, the installed entries
/// with their on flags and the progress. Output: one row per manifest entry in manifest
/// order.
pub fn pack_rows(
    manifest: &PackManifest,
    installed: &[(String, bool)],
    progress: &Progress,
) -> Vec<PackEntry> {
    let rows: Vec<PackEntry> = manifest
        .entries
        .iter()
        .map(|entry| {
            let row = installed.iter().find(|(id, _)| *id == entry.id);
            let state = match (progress.pack.get(&entry.id), row) {
                (Some(PackState::Installing), _) => PackState::Installing,
                (_, Some((_, true))) => PackState::On,
                (_, Some((_, false))) => PackState::Off,
                (Some(state), None) => state.clone(),
                (None, None) => PackState::Available,
            };
            PackEntry {
                id: entry.id.clone(),
                kind: entry.kind,
                group: entry.group,
                source: source_label(&entry.source),
                license: entry.license.clone(),
                on_by_default: entry.on_by_default,
                state,
                hooks_refused: row.and_then(|_| progress.hooks.get(&entry.id).cloned()),
            }
        })
        .collect();
    assert_eq!(rows.len(), manifest.entries.len());
    rows
}

/// The plain place an entry comes from, such as `github.com/obra/superpowers`, or
/// "Bundled with CrazyAgent". Input: the entry source. Output: the label.
pub fn source_label(source: &super::pack::PackSource) -> String {
    match source {
        super::pack::PackSource::Git { repo, .. } => repo
            .trim_start_matches("https://")
            .trim_end_matches(".git")
            .to_owned(),
        super::pack::PackSource::Bundled { .. } => "Bundled with CrazyAgent".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::{InstallError, Located, Version};
    use crate::tools::probe::Mark;
    use std::time::Duration;

    /// The mark of a probe whose first run ended, so a tool not found reads Missing.
    /// Output: the mark.
    fn settled() -> Mark {
        Mark {
            generation: 1,
            first_run_pending: false,
        }
    }

    /// SETUP-PROBE-GAP: until the login-shell probe's first run ends, a tool not found may
    /// still be on the login PATH, so its row reads Checking, which both clients read again;
    /// once that run ended it reads Missing with the resolver's line. A found tool and a
    /// refused check show as they are either way.
    #[test]
    fn a_tool_not_found_reads_checking_until_the_first_probe_run_ends() {
        let progress = |first_run_pending| Progress {
            checked: Some(Checked {
                at: Instant::now(),
                found: vec![
                    (Dependency::Tmux, Ok(Err(Dependency::Tmux.not_found()))),
                    (
                        Dependency::Uv,
                        Ok(Ok(Located {
                            version: Version::parse("0.13.5").unwrap(),
                            bundled: false,
                            note: None,
                        })),
                    ),
                    (
                        Dependency::Graphify,
                        Err(InstallError("setup_install_failed")),
                    ),
                ],
                probe: Mark {
                    generation: 0,
                    first_run_pending,
                },
            }),
            ..Progress::default()
        };
        let pending = dependency_rows(&progress(true));
        assert_eq!(pending[0].state, DependencyState::Checking);
        assert!(matches!(pending[1].state, DependencyState::Found { .. }));
        assert!(matches!(pending[2].state, DependencyState::Failed { .. }));
        let ended = dependency_rows(&progress(false));
        assert_eq!(
            ended[0].state,
            DependencyState::Missing {
                line: crate::tools::TMUX_MISSING.to_owned()
            }
        );
        assert_eq!(ended[1].state, pending[1].state);
        assert_eq!(ended[2].state, pending[2].state);
    }

    /// SETUP-PROBE-GAP: a check is due when none ended, when it is [`RECHECK_AFTER`] old, and
    /// at once when the probe's mark moved since the check read it, so a tool the login-shell
    /// probe just found shows on the next status read instead of up to 30 s later.
    #[test]
    fn a_check_is_due_again_at_once_when_the_probe_mark_moved() {
        let mark = Mark {
            generation: 3,
            first_run_pending: false,
        };
        let at = Instant::now();
        let checked = Checked {
            at,
            found: Vec::new(),
            probe: mark,
        };
        assert!(check_due(None, mark, at));
        assert!(!check_due(Some(&checked), mark, at));
        let almost = at + RECHECK_AFTER - Duration::from_millis(1);
        assert!(!check_due(Some(&checked), mark, almost));
        assert!(check_due(Some(&checked), mark, at + RECHECK_AFTER));
        let moved = Mark {
            generation: 4,
            ..mark
        };
        assert!(check_due(Some(&checked), moved, at));
    }

    /// DEC-157: a tmux found as the copy the app ships shows Bundled with its version, which
    /// clients read as `{"state":"bundled","version":...}`; any other copy stays Found.
    #[test]
    fn a_bundled_tmux_row_is_bundled_with_its_version_on_the_wire() {
        let located = |bundled| {
            Ok(Ok(Located {
                version: Version::parse("3.8").unwrap(),
                bundled,
                note: None,
            }))
        };
        let progress = Progress {
            checked: Some(Checked {
                at: Instant::now(),
                found: vec![
                    (Dependency::Tmux, located(true)),
                    (Dependency::Uv, located(false)),
                ],
                probe: settled(),
            }),
            ..Progress::default()
        };
        let rows = dependency_rows(&progress);
        let version = Version::parse("3.8").unwrap();
        assert_eq!(
            rows[0].state,
            DependencyState::Bundled {
                version: version.clone()
            }
        );
        assert_eq!(rows[1].state, DependencyState::Found { version });
        assert_eq!(
            serde_json::to_value(&rows[0].state).unwrap(),
            serde_json::json!({"state": "bundled", "version": "3.8"})
        );
    }

    /// Leader 20:08: a system tmux used in place of the bundled one is found with the note
    /// that says why, which clients read as `{"state":"system","version":...,"note":...}`.
    #[test]
    fn a_system_tmux_used_in_place_of_the_bundled_one_carries_its_note_on_the_wire() {
        use crate::tools::BundleNote;
        for note in [BundleNote::Missing, BundleNote::CannotRun] {
            let progress = Progress {
                checked: Some(Checked {
                    at: Instant::now(),
                    found: vec![(
                        Dependency::Tmux,
                        Ok(Ok(Located {
                            version: Version::parse("3.5a").unwrap(),
                            bundled: false,
                            note: Some(note),
                        })),
                    )],
                    probe: settled(),
                }),
                ..Progress::default()
            };
            let rows = dependency_rows(&progress);
            assert_eq!(
                rows[0].state,
                DependencyState::System {
                    version: Version::parse("3.5a").unwrap(),
                    note: note.line().to_owned(),
                }
            );
            assert_eq!(
                serde_json::to_value(&rows[0].state).unwrap(),
                serde_json::json!({"state": "system", "version": "3.5a", "note": note.line()})
            );
        }
    }

    #[test]
    fn a_tool_setup_cannot_install_on_this_system_is_unsupported_not_failed() {
        let unsupported = InstallError(super::super::PLATFORM_UNSUPPORTED);
        let progress = Progress {
            checked: Some(Checked {
                at: Instant::now(),
                found: Dependency::ALL.map(|dep| (dep, Err(unsupported))).to_vec(),
                probe: settled(),
            }),
            ..Progress::default()
        };
        let rows = dependency_rows(&progress);
        assert_eq!(rows.len(), Dependency::ALL.len());
        for row in rows {
            assert_eq!(row.state, DependencyState::Unsupported, "{:?}", row.name);
        }
        assert_eq!(
            serde_json::to_value(DependencyState::Unsupported).unwrap(),
            serde_json::json!({ "state": "unsupported" })
        );
        let broken = InstallError("setup_install_failed");
        let progress = Progress {
            checked: Some(Checked {
                at: Instant::now(),
                found: Dependency::ALL.map(|dep| (dep, Err(broken))).to_vec(),
                probe: settled(),
            }),
            ..Progress::default()
        };
        assert!(dependency_rows(&progress).iter().all(|row| matches!(
            &row.state,
            DependencyState::Failed { reason } if *reason == super::super::plain_reason("setup_install_failed")
        )));
    }

    /// A tool setup finds nowhere, or a script whose interpreter is missing, shows the tool
    /// resolver's owner line for that reason on its row, word for word what a launch that
    /// needs it says, such as the live master's tmux line.
    #[test]
    fn a_missing_tool_row_shows_the_resolvers_line_for_its_reason() {
        let needs = crate::tools::Missing {
            tool: "graphify".into(),
            reason: crate::tools::Reason::NeedsInterpreter,
            needs: Some("python3".into()),
            looked: Vec::new(),
            refused: None,
        };
        let progress = Progress {
            checked: Some(Checked {
                at: Instant::now(),
                found: vec![
                    (Dependency::Tmux, Ok(Err(Dependency::Tmux.not_found()))),
                    (
                        Dependency::Uv,
                        Ok(Ok(Located {
                            version: Version::parse("0.13.5").unwrap(),
                            bundled: false,
                            note: None,
                        })),
                    ),
                    (Dependency::Graphify, Ok(Err(needs.clone()))),
                ],
                probe: settled(),
            }),
            ..Progress::default()
        };
        let rows = dependency_rows(&progress);
        assert_eq!(
            rows[0].state,
            DependencyState::Missing {
                line: crate::tools::TMUX_MISSING.to_owned()
            }
        );
        assert!(matches!(rows[1].state, DependencyState::Found { .. }));
        assert_eq!(
            rows[2].state,
            DependencyState::Missing { line: needs.line() }
        );
        assert!(needs.line().contains("python3"), "{}", needs.line());
    }

    /// DEC-162: a tool the resolver found and passed over reads Refused, never Missing: the
    /// resolver's line, the file, the part others can change and the permission fix the
    /// check found, and after a fix that did not help, why; a refusal no permission change
    /// can fix has no part or fix. A tool not found still reads Missing.
    #[test]
    fn a_found_but_refused_tool_reads_refused_with_its_file_part_and_fix_not_missing() {
        let refused = |why| crate::tools::Missing {
            tool: "graphify".into(),
            reason: crate::tools::Reason::Refused,
            needs: None,
            looked: Vec::new(),
            refused: Some(Box::new(crate::tools::Refused {
                file: PathBuf::from("/Users/me/.local/share/uv/tools/graphifyy/bin/python3"),
                why,
            })),
        };
        let part = PathBuf::from("/Users/me/.local/share/uv/python/cpython-3.13.1/bin/python3.13");
        let others = refused(crate::tools::Refusal::OthersCanChange(part.clone()));
        let target = crate::tools::permission_fix::Target {
            path: PathBuf::from("/Users/me/.local/share/uv/python/cpython-3.13.1"),
            recursive: true,
        };
        let progress = |graphify: crate::tools::Missing| Progress {
            checked: Some(Checked {
                at: Instant::now(),
                found: vec![
                    (Dependency::Tmux, Ok(Err(Dependency::Tmux.not_found()))),
                    (Dependency::Graphify, Ok(Err(graphify))),
                ],
                probe: settled(),
            }),
            fixes: BTreeMap::from([(Dependency::Graphify, target.clone())]),
            ..Progress::default()
        };
        let row = |progress: &Progress| {
            dependency_rows(progress)
                .into_iter()
                .find(|row| row.name == Dependency::Graphify)
                .unwrap()
                .state
        };
        let expected = DependencyState::Refused {
            line: others.line(),
            file: "/Users/me/.local/share/uv/tools/graphifyy/bin/python3".into(),
            part: Some(part.display().to_string()),
            fix: Some(crate::setup::ToolFix {
                tree: "/Users/me/.local/share/uv/python/cpython-3.13.1".into(),
                recursive: true,
                command: "chmod -R go-w '/Users/me/.local/share/uv/python/cpython-3.13.1'".into(),
            }),
            fix_failed: None,
        };
        assert_eq!(row(&progress(others.clone())), expected);
        assert!(matches!(
            dependency_rows(&progress(others.clone()))[0].state,
            DependencyState::Missing { .. }
        ));

        let mut failed = progress(others.clone());
        failed.fix_failed.insert(
            Dependency::Graphify,
            "That folder is not yours to change.".into(),
        );
        let DependencyState::Refused {
            fix_failed, fix, ..
        } = row(&failed)
        else {
            panic!("refused row expected");
        };
        assert_eq!(
            fix_failed.as_deref(),
            Some("That folder is not yours to change.")
        );
        assert!(fix.is_some());

        let mut agents = progress(refused(crate::tools::Refusal::AgentsCanWrite));
        agents.fixes.clear();
        let DependencyState::Refused {
            part, fix, line, ..
        } = row(&agents)
        else {
            panic!("refused row expected");
        };
        assert_eq!((part, fix), (None, None));
        assert!(line.contains("agents can change it"), "{line}");
    }

    /// DEC-162: the refused state's wire shape, which both clients read: `state` refused,
    /// the line, file and part, and the fix with its tree, whether it is walked whole and
    /// the command; empty optional fields are left out.
    #[test]
    fn the_refused_state_has_one_wire_shape() {
        let state = DependencyState::Refused {
            line: "graphify can't start".into(),
            file: "/h/f".into(),
            part: Some("/h/p".into()),
            fix: Some(crate::setup::ToolFix {
                tree: "/h/t".into(),
                recursive: true,
                command: "chmod -R go-w '/h/t'".into(),
            }),
            fix_failed: None,
        };
        let wire = serde_json::to_value(&state).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "state": "refused",
                "line": "graphify can't start",
                "file": "/h/f",
                "part": "/h/p",
                "fix": {"tree": "/h/t", "recursive": true, "command": "chmod -R go-w '/h/t'"}
            })
        );
        assert_eq!(
            serde_json::from_value::<DependencyState>(wire).unwrap(),
            state
        );
        let bare = serde_json::json!({"state": "refused", "line": "l", "file": "f"});
        assert!(matches!(
            serde_json::from_value::<DependencyState>(bare).unwrap(),
            DependencyState::Refused {
                part: None,
                fix: None,
                fix_failed: None,
                ..
            }
        ));
    }
}
