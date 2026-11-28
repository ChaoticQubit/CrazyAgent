use super::sized_json;

/// A value whose JSON changes length between two serializations: a counter written as
/// `first` on the first pass and `then` after it, next to a stand-in secret.
struct Changing {
    first: u32,
    then: u32,
    passes: std::cell::Cell<u32>,
}

impl serde::Serialize for Changing {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let pass = self.passes.get();
        self.passes.set(pass + 1);
        let counter = if pass == 0 { self.first } else { self.then };
        (counter, "secret-bytes").serialize(serializer)
    }
}

#[test]
fn sized_json_refuses_a_value_that_grows_between_passes() {
    let value = Changing {
        first: 9,
        then: 10,
        passes: std::cell::Cell::new(0),
    };
    let refused = sized_json(&value).unwrap_err();
    assert_eq!(refused.0, "launch_context_unavailable");
    assert_eq!(value.passes.get(), 2);
}

#[test]
fn sized_json_refuses_a_value_that_shrinks_between_passes() {
    let value = Changing {
        first: 10,
        then: 9,
        passes: std::cell::Cell::new(0),
    };
    let refused = sized_json(&value).unwrap_err();
    assert_eq!(refused.0, "launch_context_unavailable");
    assert_eq!(value.passes.get(), 2);
}

#[test]
fn sized_json_writes_a_steady_value_as_compact_json() {
    let value = Changing {
        first: 9,
        then: 9,
        passes: std::cell::Cell::new(0),
    };
    assert_eq!(sized_json(&value).unwrap(), r#"[9,"secret-bytes"]"#);
}
