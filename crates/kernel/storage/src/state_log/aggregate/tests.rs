use super::*;

#[test]
fn a_name_serializes_as_its_string() {
    const NAME: AggregateName = AggregateName::new("worktrees");
    assert_eq!(NAME.name(), "worktrees");
    assert_eq!(serde_json::to_string(&NAME).unwrap(), "\"worktrees\"");
}

#[test]
fn the_longest_name_and_every_allowed_byte_are_kept() {
    let longest = "a".repeat(AGGREGATE_BYTES_MAX);
    let longest: &'static str = Box::leak(longest.into_boxed_str());
    assert_eq!(
        AggregateName::new(longest).name().len(),
        AGGREGATE_BYTES_MAX
    );
    assert_eq!(AggregateName::new("team_2").name(), "team_2");
}

#[test]
#[should_panic(expected = "an aggregate name is 1 to 64 bytes")]
fn an_empty_name_is_refused() {
    let _ = AggregateName::new("");
}

#[test]
#[should_panic(expected = "an aggregate name is 1 to 64 bytes")]
fn a_name_over_the_bound_is_refused() {
    let over: &'static str = Box::leak("a".repeat(AGGREGATE_BYTES_MAX + 1).into_boxed_str());
    let _ = AggregateName::new(over);
}

#[test]
#[should_panic(expected = "an aggregate name holds only [a-z0-9_]")]
fn a_name_with_another_byte_is_refused() {
    let _ = AggregateName::new("Worktrees");
}
