/// Tells whether pinned file identities, as (device, inode) pairs, still name the live
/// files. Every inode must match, and every device too, unless the one volume holding all
/// of them was renumbered at mount (macOS can give a volume another st_dev after a
/// restart): then all pinned devices share one value and all live devices share another.
/// The caller pairs each pinned identity with the live one of the same file; nothing is
/// rewritten. Inputs: pinned and live pairs, in the same order and of the same length.
/// Output: true when they name the same files.
pub fn same_files(pinned: &[(u64, u64)], live: &[(u64, u64)]) -> bool {
    assert_eq!(pinned.len(), live.len(), "file identities are paired");
    let one_device = |pairs: &[(u64, u64)]| pairs.windows(2).all(|pair| pair[0].0 == pair[1].0);
    pinned
        .iter()
        .zip(live)
        .all(|(pinned, live)| pinned.1 == live.1)
        && (pinned == live || (one_device(pinned) && one_device(live)))
}

#[cfg(all(test, unix))]
#[path = "file_identity/tests.rs"]
mod tests;
