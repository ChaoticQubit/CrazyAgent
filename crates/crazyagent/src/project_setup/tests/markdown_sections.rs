use super::*;

#[test]
fn markdown_sections_are_selected_by_heading() {
    let report = "# Market\nintro\n## Size\nbig\n### Detail\nfine\n## Trends\nup\n";
    assert_eq!(section(report, None).unwrap(), report.trim());
    assert_eq!(
        section(report, Some("Size")).unwrap(),
        "## Size\nbig\n### Detail\nfine"
    );
    assert_eq!(section(report, Some("Trends")).unwrap(), "## Trends\nup");
    assert!(section(report, Some("Absent")).is_none());
}
