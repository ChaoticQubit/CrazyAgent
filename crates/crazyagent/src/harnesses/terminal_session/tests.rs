use super::*;

fn unframed(parts: &[String]) -> String {
    let joined = parts.concat();
    assert!(joined.starts_with(PASTE_START), "{joined:?}");
    assert!(joined.ends_with(PASTE_END), "{joined:?}");
    joined[PASTE_START.len()..joined.len() - PASTE_END.len()].to_owned()
}

#[test]
fn a_prompt_is_one_bracketed_paste() {
    let parts = paste("Fix the build\nthen run\ttests");
    assert_eq!(
        parts,
        vec!["\x1b[200~Fix the build\nthen run\ttests\x1b[201~".to_owned()]
    );
}

#[test]
fn control_characters_cannot_end_the_paste_or_press_keys() {
    let parts = paste("a\x1b[201~\r\nb\rc\x03d\x7fe\u{9b}f\u{0}g");
    assert_eq!(unframed(&parts), "a[201~\nb\ncdefg");
    assert_eq!(parts.concat().matches(PASTE_END).count(), 1);
}

#[test]
fn a_long_prompt_is_split_into_host_inputs_on_character_boundaries() {
    let prompt = "é".repeat(INPUT_BYTES_MAX);
    let parts = paste(&prompt);
    assert!(parts.len() > 2);
    assert!(parts.iter().all(|part| part.len() <= INPUT_BYTES_MAX));
    assert_eq!(unframed(&parts), prompt);
}

#[test]
fn an_empty_prompt_is_still_one_paste() {
    assert_eq!(paste(""), vec!["\x1b[200~\x1b[201~".to_owned()]);
}
