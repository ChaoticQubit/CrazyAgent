use super::Grants;
use std::sync::Mutex;

/// Returns the size of one element of the vector a `Grants` field holds. Input: a function
/// that picks the field. Output: the element's size in bytes.
fn element<T>(_: for<'a> fn(&'a Grants) -> &'a Mutex<Vec<T>>) -> usize {
    size_of::<T>()
}

/// A live grant is held behind a pointer, so a swap or a growth of the list moves only the
/// pointer and leaves no copy of a grant's tokens in the list's memory.
#[test]
fn a_live_grant_is_held_behind_a_pointer() {
    assert_eq!(element(|grants| &grants.live), size_of::<usize>());
}
