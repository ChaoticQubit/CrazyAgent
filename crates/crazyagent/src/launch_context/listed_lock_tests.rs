use super::*;

/// A listed launch holds its private home's lock for as long as its guard lives: a second
/// listed launch into the same home is refused as busy and writes nothing, and once the
/// first guard is dropped the home takes a launch again.
#[test]
fn a_second_listed_launch_into_a_held_home_is_refused_as_busy_until_the_first_ends() {
    let (_folder, home) = private_home();
    let mcp = home.join("agent").join("mcp.json");
    let first = context().install_listed(&manifest(BLOCKS), &home, true);
    assert!(first.is_ok(), "{:?}", first.as_ref().err());
    let written = fs::read(&mcp).unwrap();

    let second = context().install_listed(&manifest(BLOCKS), &home, true);

    assert_eq!(second.err(), Some(failure("launch_home_busy")));
    assert_eq!(fs::read(&mcp).unwrap(), written);
    drop(first);
    let third = context().install_listed(&manifest(BLOCKS), &home, true);
    assert!(third.is_ok(), "{:?}", third.as_ref().err());
}
