/// The text of a fixed absolute fixture path on this system. Unix keeps the path as
/// written; Windows, where a path rooted at `/` is relative, puts it on drive `C:` with `\`
/// between names. Input: a path written from `/`. Output: the absolute path text.
pub fn unit_path(unix: &str) -> String {
    assert!(unix.starts_with('/') && !unix.contains('\\'));
    if cfg!(windows) {
        format!("C:{}", unix.replace('/', "\\"))
    } else {
        unix.to_owned()
    }
}
