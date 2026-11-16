use super::*;
use crate::github::GitHubNewRepo;
use crate::github::api;

fn plan(owner: &str, name: &str, description: &str) -> GitHubPublishPlan {
    GitHubPublishPlan {
        owner: owner.to_owned(),
        name: name.to_owned(),
        description: description.to_owned(),
        private: true,
        docs: false,
    }
}

fn start_takes(plan: &GitHubPublishPlan) -> bool {
    api::check_new(&GitHubNewRepo {
        owner: plan.owner.clone(),
        name: plan.name.clone(),
        description: plan.description.clone(),
        private: plan.private,
    })
    .is_ok()
}

#[test]
fn the_repository_name_is_the_project_name_made_safe_for_github() {
    let cut = "a".repeat(96);
    let cases = [
        ("My Project".to_owned(), "My-Project".to_owned()),
        ("  hello   world!! ".to_owned(), "hello-world".to_owned()),
        ("..hidden".to_owned(), "hidden".to_owned()),
        ("-dash_ok.v2".to_owned(), "dash_ok.v2".to_owned()),
        ("repo.git".to_owned(), "repo".to_owned()),
        ("x.git.git".to_owned(), "x".to_owned()),
        ("caf\u{e9} app".to_owned(), "caf-app".to_owned()),
        ("\u{65e5}\u{672c}".to_owned(), String::new()),
        (String::new(), String::new()),
        ("a".repeat(150), "a".repeat(100)),
        (format!("{cut}.gitx"), cut.clone()),
    ];
    for (project, want) in cases {
        let name = default_repo_name(&project);
        assert_eq!(name, want, "{project:?}");
        if !name.is_empty() {
            let proposed = plan("me", &name, "");
            assert_eq!(plan_problem(&proposed), None, "{name:?}");
            assert!(start_takes(&proposed), "{name:?}");
        }
    }
}

#[test]
fn the_description_is_the_about_text_on_one_line_within_350_characters() {
    let accent = "\u{e9}";
    let face = "\u{1f600}";
    let cases = [
        (
            "A  tool\n\nfor\tthings ".to_owned(),
            "A tool for things".to_owned(),
        ),
        ("bell\u{7}here".to_owned(), "bell here".to_owned()),
        (accent.repeat(400), accent.repeat(350)),
        (face.repeat(400), face.repeat(350)),
        (format!("{} b", "a".repeat(349)), "a".repeat(349)),
        (String::new(), String::new()),
    ];
    for (about, want) in cases {
        let description = default_description(&about);
        assert_eq!(description, want, "{about:?}");
        let proposed = plan("me", "site", &description);
        assert_eq!(plan_problem(&proposed), None);
        assert!(start_takes(&proposed));
    }
}

#[test]
fn the_plan_problem_names_the_field_start_refuses() {
    let cases = [
        (plan("me", "site", "A tool"), None),
        (plan("me", "", ""), Some(GitHubPlanField::Name)),
        (plan("me", "bad name", ""), Some(GitHubPlanField::Name)),
        (plan("me", ".x", ""), Some(GitHubPlanField::Name)),
        (plan("me", "-x", ""), Some(GitHubPlanField::Name)),
        (plan("me", "x.git", ""), Some(GitHubPlanField::Name)),
        (plan("me", "a/b", ""), Some(GitHubPlanField::Name)),
        (
            plan("me", &"a".repeat(101), ""),
            Some(GitHubPlanField::Name),
        ),
        (plan("me", &"a".repeat(100), ""), None),
        (plan("", "site", ""), Some(GitHubPlanField::Owner)),
        (plan("-bad", "site", ""), Some(GitHubPlanField::Owner)),
        (
            plan(&"a".repeat(40), "site", ""),
            Some(GitHubPlanField::Owner),
        ),
        (plan("a_b", "site", ""), Some(GitHubPlanField::Owner)),
        (plan(&"a".repeat(39), "site", ""), None),
        (
            plan("me", "site", &"x".repeat(351)),
            Some(GitHubPlanField::Description),
        ),
        (
            plan("me", "site", "a\u{7}b"),
            Some(GitHubPlanField::Description),
        ),
        (
            plan("me", "site", "line\nbreak"),
            Some(GitHubPlanField::Description),
        ),
        (
            plan("me", "site", "a\tb"),
            Some(GitHubPlanField::Description),
        ),
        (plan("me", "site", &"\u{1f600}".repeat(350)), None),
    ];
    for (proposed, want) in cases {
        assert_eq!(plan_problem(&proposed), want, "{proposed:?}");
        assert_eq!(want.is_none(), start_takes(&proposed), "{proposed:?}");
    }
}
