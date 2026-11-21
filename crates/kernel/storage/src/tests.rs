use super::*;

#[test]
fn a_storage_error_displays_its_code_unchanged() {
    let error = StorageError("state_log_redact_missing".to_owned());
    assert_eq!(error.to_string(), "state_log_redact_missing");
    let boxed: Box<dyn std::error::Error> = Box::new(error.clone());
    assert_eq!(boxed.to_string(), error.0);
}
