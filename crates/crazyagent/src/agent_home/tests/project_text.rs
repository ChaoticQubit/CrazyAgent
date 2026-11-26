use super::*;

#[test]
fn a_project_text_made_only_of_repeats_adds_no_project_heading() {
    let joined = render("- Use TDD.\n", "- Use TDD.\n\n", Repeated::LeaveOut);
    assert!(!joined.contains("## Project instructions"));
    let headed = render(
        "## Rules\n\n- Use TDD.\n",
        "## Rules\n\n- Use TDD.\n\n---\n",
        Repeated::LeaveOut,
    );
    assert!(!headed.contains("## Project instructions"), "{headed}");
    assert_eq!(headed.matches("- Use TDD.").count(), 1);
}
