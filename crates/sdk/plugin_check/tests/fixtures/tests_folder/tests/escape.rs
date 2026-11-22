#[test]
fn reads() {
    assert!(std::fs::read("x").is_err());
}
