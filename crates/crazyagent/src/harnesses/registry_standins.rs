use super::{Harness, Registry};
use std::sync::LazyLock;

/// The Task 9 stand-in manifests a test build's `Registry::get` also finds, so in-crate rows
/// reach a listed harness kind: the terminal one. `Registry::all` never lists them, so
/// catalog, count and snapshot rows see the bundled harnesses only.
const FILES: [(&str, &str); 1] = [(
    "standin-screen",
    include_str!("../../tests/fixtures/harness/standin-screen.yaml"),
)];

/// Finds a stand-in harness. Input: its id. Output: the harness, or none for a name no
/// stand-in has.
pub(crate) fn get(name: &str) -> Option<&'static Harness> {
    static BUILT: LazyLock<Registry> =
        LazyLock::new(|| Registry::build(&FILES).expect("every stand-in manifest loads"));
    assert_eq!(BUILT.all().len(), FILES.len());
    BUILT.position(name).map(|index| &BUILT.all()[index])
}
