use super::*;

fn repo_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(crate::research::runner::setup_git());
    command
        .args(["init", "-q", "--template="])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(dir.path());
    let status = crate::spawn_gate::status(&mut command);
    assert!(status.is_ok_and(|status| status.success()), "git init");
    for (path, text) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

fn eight_full_docs() -> Vec<(String, String)> {
    let filler = format!("# Notes\n\n{}", "plain words\n".repeat(600));
    [
        "ci-pipelines",
        "data-model",
        "deploy",
        "design",
        "dev-setup",
        "domain",
        "e2e",
        "edge-authentication",
    ]
    .iter()
    .map(|name| (format!("docs/{name}.md"), filler.clone()))
    .collect()
}

fn borrowed(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect()
}

#[test]
fn a_research_file_ninth_by_name_is_read_before_the_text_budget_runs_out() {
    let mut files = eight_full_docs();
    files.push((
        "docs/research-competitors.md".into(),
        "# Competitor and market research\n\n## Summary\n\n## Comparison table\n\n## References\n"
            .into(),
    ));
    let repo = repo_with(&borrowed(&files));
    let (documents, _) = read(repo.path(), None, &[]).unwrap();
    assert_eq!(documents[0].path, "docs/research-competitors.md");
    assert_eq!(documents[0].kind, FoundKind::Research);
}

#[test]
fn scholarscopes_ignored_roadmap_is_read_first_then_its_research() {
    let roadmap = "# ScholarScope - phase 1 delivery plan (revision 2)\n\n## 0.0 What changed from the previous plan\n\n## 1. Phase 1 product scope\n\n## 7. Sequencing\n\n### M0 Foundations\n\n### M1 Identity\n\n### M2 Catalog core (the product loop)\n\n### M3 Discovery\n\n### M4 Clients\n\n### M5 Launch readiness\n\n### M6 Hardening\n\n## 8. Decision holds\n";
    let research = "# ScholarScope - competitor and market research for the marketing landing page\n\n## Summary\n\n## Comparison table\n\n## Where ScholarScope is genuinely different\n\n## References\n";
    let ticket_plan = format!(
        "# Login plan\n\n## Phase 1\n\n## Phase 2\n\n{}",
        "- [ ] Step\n".repeat(40)
    );
    let mut files = eight_full_docs();
    files.push((
        ".gitignore".into(),
        "research/\n.*/\nScholarScope-handoff.md\n".into(),
    ));
    files.push((
        "research/docs/ss-phase1-revision/report.md".into(),
        roadmap.into(),
    ));
    files.push(("docs/research-competitors.md".into(), research.into()));
    files.push((
        "docs/superpowers/plans/2026-10-01-login.md".into(),
        ticket_plan,
    ));
    files.push((
        ".agent/handoff.md".into(),
        "# Handoff\n\n## START HERE 2026-10-24\n".into(),
    ));
    let repo = repo_with(&borrowed(&files));
    let (documents, _) = read(repo.path(), None, &[]).unwrap();
    assert_eq!(
        documents[0].path,
        "research/docs/ss-phase1-revision/report.md"
    );
    assert_eq!(documents[0].kind, FoundKind::Roadmap);
    assert_eq!(
        documents[0].marks[..2],
        ["M0 Foundations".to_owned(), "M1 Identity".to_owned()]
    );
    assert_eq!(documents[1].path, "docs/research-competitors.md");
    assert_eq!(documents[1].kind, FoundKind::Research);
}

#[test]
fn heading_kind_tells_a_milestone_plan_from_a_ticket_plan_and_from_research() {
    let milestones = "# Delivery plan\n\n## 7. Sequencing\n\n### M0 Foundations\n\n### M1 Identity\n\n### M2 Catalog core\n";
    let tickets = format!(
        "# Login plan\n\n## Phase 1\n\n## Phase 2\n\n{}",
        "- [ ] Step\n".repeat(40)
    );
    let research = "# Competitor and market research\n\n## Comparison table\n\n## References\n";
    let fenced = "# Notes\n\n```sh\n# M0 build\n# M1 test\n# M2 ship\n```\n";
    assert_eq!(heading_kind(milestones).0, FoundKind::Roadmap);
    assert_eq!(heading_kind(&tickets).0, FoundKind::Other);
    assert_eq!(heading_kind(research).0, FoundKind::Research);
    assert_eq!(heading_kind(fenced), (FoundKind::Other, 0, Vec::new()));
}

#[test]
fn heading_kind_bounds_its_score_and_marks() {
    let many: String = (0..200)
        .map(|number| format!("## M{number} {}\n", "x".repeat(300)))
        .collect();
    let (kind, score, marks) = heading_kind(&many);
    assert_eq!(kind, FoundKind::Roadmap);
    assert_eq!(score, (MILESTONES_COUNTED_MAX * 6) as u32);
    assert_eq!(marks.len(), MARKS_MAX);
    assert!(marks.iter().all(|mark| mark.len() <= MARK_BYTES_MAX));
    assert_eq!(
        heading_kind(&"## Roadmap\n".repeat(200)),
        (FoundKind::Roadmap, SCORE_MAX, Vec::new())
    );
    let linked: String = "[a](https://example.com) ".repeat(8 * 200);
    assert_eq!(
        heading_kind(&format!("# Market research\n\n{linked}\n")),
        (FoundKind::Research, 3 + 16, Vec::new())
    );
}

#[test]
fn ignored_files_that_are_not_markdown_or_look_secret_stay_unread() {
    let repo = repo_with(&[
        (".gitignore", "notes.txt\nresearch/\n"),
        ("notes.txt", "# Plan\n\n### M0 Start\n\n### M1 Ship\n"),
        (
            "research/secrets-plan.md",
            "# Plan\n\n### M0 Start\n\n### M1 Ship\n",
        ),
    ]);
    let (documents, unread) = read(repo.path(), None, &[]).unwrap();
    assert!(documents.is_empty(), "{documents:?}");
    assert!(unread.is_empty(), "{unread:?}");
}

#[test]
fn a_roadmap_in_a_hidden_folder_is_found_but_git_internals_and_the_engine_folder_are_not() {
    let plan = "# Release plan\n\n### M0 Start\n\n### M1 Beta\n\n### M2 Launch\n";
    let repo = repo_with(&[
        (".agent/reports/plan.md", plan),
        (".crazyagent/worktrees/team-1/plan.md", plan),
        (".crazyagent/research/agents/run-1/notes.md", plan),
        (".crazyagent/context/plan.md", plan),
        (".git/plan.md", plan),
        (".hidden.md", plan),
    ]);
    let (documents, unread) = read(repo.path(), None, &[]).unwrap();
    let paths: Vec<&str> = documents
        .iter()
        .map(|document| document.path.as_str())
        .collect();
    assert_eq!(paths, [".agent/reports/plan.md"]);
    assert!(unread.is_empty(), "{unread:?}");
}

#[test]
fn a_hidden_folder_reached_through_a_link_is_not_walked() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("plan.md"),
        "# Plan\n\n### M0 Start\n\n### M1 Ship\n",
    )
    .unwrap();
    let repo = repo_with(&[("README.md", "# App\n")]);
    std::os::unix::fs::symlink(outside.path(), repo.path().join(".linked")).unwrap();
    let (documents, unread) = read(repo.path(), None, &[]).unwrap();
    assert!(
        documents
            .iter()
            .all(|document| !document.path.starts_with(".linked/"))
    );
    assert!(unread.iter().all(|(path, _)| !path.starts_with(".linked/")));
}

#[test]
fn a_large_hidden_folder_never_starves_the_visible_documents() {
    let repo = repo_with(&[(
        "research/plan.md",
        "# Plan\n\n### M0 Start\n\n### M1 Ship\n",
    )]);
    std::fs::create_dir_all(repo.path().join("docs/.cache")).unwrap();
    for number in 0..ENTRIES_MAX + 10 {
        std::fs::write(repo.path().join(format!("docs/.cache/f{number}.md")), "x\n").unwrap();
    }
    let (documents, _) = read(repo.path(), None, &[]).unwrap();
    assert_eq!(documents[0].path, "research/plan.md");
}

#[test]
fn a_readme_line_naming_ignored_markdown_stays_and_one_naming_an_ignored_env_file_goes() {
    let repo = repo_with(&[
        (".gitignore", "research/\n.env.local\n"),
        ("research/plan.md", "# Plan\n"),
        (".env.local", "X=1\n"),
    ]);
    let mut evidence = Evidence {
        readme: Some((
            "README.md".into(),
            "See research/plan.md\nCopy .env.local\n".into(),
        )),
        ..Evidence::default()
    };
    screen(repo.path(), &mut evidence, &[]);
    assert_eq!(evidence.readme.unwrap().1, "See research/plan.md\n");
}

#[test]
fn the_prompt_shows_the_local_scan_first_inside_each_documents_fence() {
    let document = |path: &str, kind: FoundKind, marks: &[&str]| Document {
        path: path.into(),
        size: 10,
        text: "# T".into(),
        cut: false,
        kind,
        marks: marks.iter().map(|mark| (*mark).to_owned()).collect(),
    };
    let text = crate::onboarding::draft::documents_section(
        &[
            document("a.md", FoundKind::Roadmap, &["M0 Start", "M1 Ship"]),
            document("b.md", FoundKind::Research, &[]),
            document("c.md", FoundKind::Other, &[]),
        ],
        &[],
        false,
    );
    assert!(
        text.contains(
            "### a.md (10 bytes)\n\n```\nLocal scan: roadmap (M0 Start; M1 Ship)\n# T\n```"
        ),
        "{text}"
    );
    assert!(
        text.contains("### b.md (10 bytes)\n\n```\nLocal scan: research\n# T\n```"),
        "{text}"
    );
    assert!(
        text.contains("### c.md (10 bytes)\n\n```\n# T\n```"),
        "{text}"
    );
    assert_eq!(text.matches("Local scan").count(), 2);
}

#[test]
fn dependency_folders_and_python_environments_are_not_walked() {
    let plan = "# Roadmap\n\n### M0 Start\n\n### M1 Beta\n\n### M2 Launch\n";
    let repo = repo_with(&[
        ("docs/roadmap.md", plan),
        (".venv/pyvenv.cfg", "home = /usr/bin\n"),
        (".venv/lib/python3.12/site-packages/pkg/CHANGELOG.md", plan),
        (".tox/py312/README.md", plan),
        (".cargo/registry/notes.md", plan),
        ("env/pyvenv.cfg", "home = /usr/bin\n"),
        ("env/notes/plan.md", plan),
        ("lib/site-packages/pkg/README.md", plan),
    ]);
    let (documents, unread) = read(repo.path(), None, &[]).unwrap();
    let paths: Vec<&str> = documents
        .iter()
        .map(|document| document.path.as_str())
        .collect();
    assert_eq!(paths, ["docs/roadmap.md"]);
    assert!(unread.is_empty(), "{unread:?}");
}

#[test]
fn a_changelogs_version_headings_are_not_milestones() {
    let changelog = concat!(
        "# Changelog\n\n## v3.0.0 - 2025-01-01\n\n",
        "- Faster ([#12](https://github.com/o/r/pull/12))\n\n",
        "## v2.1.0 - 2024-06-01\n\n## V1.0.0 - 2024-01-01\n\n",
        "## Release 0.9.0\n\n## Release 0.8.1\n",
    );
    assert_eq!(heading_kind(changelog), (FoundKind::Other, 0, Vec::new()));
    let versions = "## V1 - MVP\n\n## V2: Teams\n\n## v3. Scale\n\n## Release 4 Launch\n";
    let (kind, score, marks) = heading_kind(versions);
    assert_eq!(kind, FoundKind::Roadmap);
    assert_eq!(score, 4 * 6);
    assert_eq!(marks.len(), 4, "{marks:?}");
}

#[test]
fn a_heading_mark_naming_a_hidden_folder_path_or_a_secret_like_file_never_reaches_the_prompt() {
    let repo = repo_with(&[("README.md", "# App\n")]);
    let mut evidence = Evidence {
        documents: vec![Document {
            path: "docs/plan.md".into(),
            size: 1,
            text: "# Plan\n".into(),
            cut: false,
            kind: FoundKind::Roadmap,
            marks: vec![
                "M1 Identity".into(),
                "M3: move config/credentials.json into the vault".into(),
                "Phase 2: publish client-x/contract.md".into(),
                "M4 Ship docs/guide.md".into(),
            ],
        }],
        ..Evidence::default()
    };
    screen(repo.path(), &mut evidence, &["client-x".to_owned()]);
    assert_eq!(
        evidence.documents[0].marks,
        ["M1 Identity", "M4 Ship docs/guide.md"]
    );
    let prompt =
        crate::onboarding::draft::prompt(&evidence, &crate::onboarding::MappingFacts::default());
    assert!(
        prompt.contains("Local scan: roadmap (M1 Identity; M4 Ship docs/guide.md)"),
        "{prompt}"
    );
    assert!(!prompt.contains("credentials"), "{prompt}");
    assert!(!prompt.contains("client-x"), "{prompt}");
}

/// A mapping answer that names all six setup kinds with files keeps the five research kinds
/// the prompt offers and no roadmap planning group, so the onboarding thread goes on.
#[test]
fn an_answer_naming_all_six_setup_kinds_keeps_the_five_research_kinds_and_no_planning_group() {
    let named = |kind: &ResearchKind| serde_json::json!({"kind": kind.name(), "files": [format!("docs/{}.md", kind.name())]});
    let items: Vec<serde_json::Value> = ResearchKind::CHOICES.iter().map(named).collect();
    assert_eq!(items.len(), 6);
    let found = research(&serde_json::Value::Array(items));
    let kinds: Vec<ResearchKind> = found.iter().map(|known| known.kind).collect();
    assert_eq!(kinds, ResearchKind::ALL);
    for known in &found {
        assert_eq!(known.files, [format!("docs/{}.md", known.kind.name())]);
    }
    let planning = serde_json::json!([{"kind": "roadmap_planning", "files": ["docs/next.md"]}]);
    assert!(research(&planning).is_empty());
    let twice = serde_json::json!([
        {"kind": " market_research ", "files": ["docs/a.md"]},
        {"kind": "roadmap_planning", "files": ["docs/next.md"]},
        {"kind": "market_research", "files": ["docs/b.md"]},
        {"kind": "made_up", "files": ["docs/c.md"]}
    ]);
    let merged = research(&twice);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].kind, ResearchKind::MarketResearch);
    assert_eq!(merged[0].files, ["docs/a.md", "docs/b.md"]);
}

fn fixture_git(repo: &Path, args: &[&str]) {
    let mut command = std::process::Command::new(crate::research::runner::setup_git());
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(repo);
    let status = crate::spawn_gate::status(&mut command);
    assert!(status.is_ok_and(|status| status.success()), "git {args:?}");
}

#[test]
fn a_listing_never_runs_the_promisor_remote_a_hostile_repository_names() {
    let repo = repo_with(&[("notes.md", "# Notes\n")]);
    let outside = tempfile::tempdir().unwrap();
    let fetched = outside.path().join("fetched");
    assert!(!fetched.display().to_string().contains(' '));
    let remote = format!("ext::sh -c touch% {}% ;% false", fetched.display());
    for (key, value) in [
        ("core.repositoryformatversion", "1"),
        ("extensions.partialClone", "origin"),
        ("remote.origin.url", remote.as_str()),
        ("remote.origin.promisor", "true"),
        ("protocol.ext.allow", "always"),
    ] {
        fixture_git(repo.path(), &["config", key, value]);
    }
    let absent = format!("100644,{},.gitignore", "5".repeat(40));
    fixture_git(
        repo.path(),
        &["update-index", "--add", "--cacheinfo", &absent],
    );
    fixture_git(
        repo.path(),
        &["update-index", "--skip-worktree", ".gitignore"],
    );

    let kept = kept_out(repo.path());
    let listed = untracked(repo.path());

    assert!(
        !fetched.exists(),
        "a listing ran the command the repository's promisor remote names"
    );
    assert_eq!(kept, Some(Vec::new()));
    assert_eq!(listed, Some(vec!["notes.md".to_owned()]));
}

#[test]
fn the_project_git_never_fetches_a_missing_object_and_allows_no_transport() {
    let command = git(Path::new("/"), &["status"]);
    let set: Vec<(String, Option<String>)> = command
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect();

    assert!(set.contains(&("GIT_NO_LAZY_FETCH".into(), Some("1".into()))));
    assert!(set.contains(&("GIT_ALLOW_PROTOCOL".into(), Some(String::new()))));
}
