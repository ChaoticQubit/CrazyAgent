use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crazyagent_cap::File;

use crate::{CapgenError, load, wit};

/// An in-test copy of `caps/settings.cap`, so a new `.cap` file under `caps/` leaves the
/// exact-text row unchanged.
const SETTINGS: &str = "enum OnQuit { keep_running, stop }
enum LeftRunning { after_hours, never }
type QuitSettings { on_quit: OnQuit, left_running: LeftRunning, hours: u16 }
capability settings.save_quit(settings: QuitSettings) -> unit
  tier: kernel
  needs: settings.write
  emits: AgentQuitSaved { settings: QuitSettings }
";

/// The `sdk.wit` that `wit` writes from [`SETTINGS`].
const SETTINGS_WIT: &str = "package crazyagent:sdk;

interface sdk-types {
  record not-granted {
    capability: string,
    needs: string,
  }

  variant refusal {
    not-granted(not-granted),
    tier-too-high(string),
    rejected(string),
    failed(string),
  }
}

interface settings-types {
  enum on-quit {
    keep-running,
    stop,
  }

  enum left-running {
    after-hours,
    never,
  }

  record quit-settings {
    on-quit: on-quit,
    left-running: left-running,
    hours: u16,
  }
}

interface settings {
  use sdk-types.{refusal};
  use settings-types.{quit-settings};

  save-quit: func(settings: quit-settings) -> result<_, refusal>;
}

world plugin {
  import settings;
}
";

/// Each capgen fixture folder with the path of its checked-in WIT golden below `capgen`.
const FIXTURES: [(&str, &str); 3] = [
    ("fixture", "fixture/fixture.wit"),
    ("fixture_two", "fixture_two/sdk.wit"),
    ("fixture_wire", "fixture_wire/sdk.wit"),
];

/// Names a folder of the sdk tree. Input: its path below `crates/sdk`. Output: the folder.
fn sdk(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(path)
}

/// Parses `.cap` sources named by their stems. Input: stems with sources.
/// Output: the stems with their parsed files; panics on a grammar refusal.
fn parsed<'s>(sources: &[(&'s str, &str)]) -> Vec<(&'s str, File)> {
    sources
        .iter()
        .map(|(stem, source)| {
            (
                *stem,
                crazyagent_cap::parse_as(stem, source).expect("parses"),
            )
        })
        .collect()
}

/// Writes the one WIT file for loaded `.cap` files. Input: stems with parsed files.
/// Output: the `sdk.wit` text; panics on a refusal or on any other file.
fn only_wit(files: &[(&str, File)]) -> String {
    let out = wit(files).expect("generates");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "sdk.wit");
    out[0].1.clone()
}

/// Writes the WIT of one folder of `.cap` files. Input: the folder below `crates/sdk`.
/// Output: the `sdk.wit` text.
fn folder_wit(folder: &str) -> String {
    let loaded = load(&sdk(folder)).expect("the folder loads");
    let borrowed: Vec<(&str, File)> = loaded
        .iter()
        .map(|(stem, file)| (stem.as_str(), file.clone()))
        .collect();
    only_wit(&borrowed)
}

/// Runs `wit` over sources that must be refused. Input: stems with sources.
/// Output: the refusal; panics when `wit` accepts them.
fn refused(sources: &[(&str, &str)]) -> CapgenError {
    match wit(&parsed(sources)) {
        Ok(files) => panic!("generated, expected a refusal: {files:?}"),
        Err(error) => error,
    }
}

/// Parses one WIT text and checks its package and world. Inputs: a label for failures and
/// the text. Output: nothing; panics when the text does not parse or is misnamed.
fn parses(label: &str, text: &str) {
    let mut resolve = wit_parser::Resolve::new();
    let id = resolve
        .push_str("sdk.wit", text)
        .unwrap_or_else(|error| panic!("{label} does not parse: {error:?}\n{text}"));
    let package = &resolve.packages[id];
    assert_eq!(package.name.to_string(), "crazyagent:sdk", "{label}");
    assert!(package.worlds.contains_key("plugin"), "{label}");
}

#[test]
fn wit_settings_text_is_exact() {
    assert_eq!(only_wit(&parsed(&[("settings", SETTINGS)])), SETTINGS_WIT);
}

#[test]
fn wit_fixtures_generate_the_checked_in_goldens() {
    for (folder, golden) in FIXTURES {
        let checked_in =
            fs::read_to_string(sdk(&format!("capgen/{golden}"))).expect("the golden reads");
        assert_eq!(
            folder_wit(&format!("capgen/{folder}")),
            checked_in,
            "{folder}"
        );
    }
}

#[test]
fn wit_a_type_and_function_clash_gets_the_stem_alias() {
    let text = folder_wit("capgen/fixture");
    assert!(text.contains("\ninterface fixture-types {\n"), "{text}");
    assert!(text.contains("\n  record pair {\n"), "{text}");
    let uses = text
        .lines()
        .find(|line| line.starts_with("  use fixture-types.{"))
        .unwrap_or_else(|| panic!("fixture uses its types: {text}"));
    assert!(uses.contains("keyed, pair as fixture-pair"), "{uses}");
    assert!(
        text.contains("\n  pair: func(pair: fixture-pair, keyed: keyed) -> result<_, refusal>;\n"),
        "{text}"
    );
    parses("fixture", &text);
}

#[test]
fn wit_a_non_clashing_use_has_no_alias() {
    let text = only_wit(&parsed(&[("settings", SETTINGS)]));
    assert!(
        text.contains("\n  use settings-types.{quit-settings};\n"),
        "{text}"
    );
    assert!(!text.contains(" as "), "{text}");
    let fixture = folder_wit("capgen/fixture");
    assert!(!fixture.contains("keyed as"), "{fixture}");
}

#[test]
fn wit_an_alias_that_clashes_is_refused() {
    let function = refused(&[(
        "x",
        "type Pair { a: u16 }\n\
         capability x.pair(pair: Pair) -> unit\n  tier: t1\n  needs: x.y\n\
         capability x.x_pair(a: u16) -> unit\n  tier: t1\n  needs: x.y\n",
    )]);
    assert_eq!(function.code, "capgen_name_collision", "{function:?}");
    assert_eq!(function.detail, "wit: x-pair");
    let foreign = refused(&[
        (
            "a",
            "enum Mode { x }\nenum BMode { y }\ntype T { m: b.Mode }",
        ),
        ("b", "enum Mode { z }"),
    ]);
    assert_eq!(foreign.code, "capgen_name_collision", "{foreign:?}");
    assert_eq!(foreign.detail, "wit: b-mode");
    assert_eq!(foreign.file, "a.cap");
}

#[test]
fn wit_generation_is_byte_stable_in_any_file_order() {
    let net = fs::read_to_string(sdk("capgen/fixture_two/net.cap")).expect("net.cap reads");
    let secrets =
        fs::read_to_string(sdk("capgen/fixture_two/secrets.cap")).expect("secrets.cap reads");
    let forward = only_wit(&parsed(&[("net", &net), ("secrets", &secrets)]));
    let backward = only_wit(&parsed(&[("secrets", &secrets), ("net", &net)]));
    assert_eq!(forward, backward);
    assert_eq!(folder_wit("capgen/fixture_two"), forward);
    assert_eq!(
        folder_wit("capgen/fixture_two"),
        folder_wit("capgen/fixture_two")
    );
}

#[test]
fn every_generated_wit_parses() {
    parses("caps", &folder_wit("caps"));
    parses("settings", &only_wit(&parsed(&[("settings", SETTINGS)])));
    for (folder, _) in FIXTURES {
        parses(folder, &folder_wit(&format!("capgen/{folder}")));
    }
}

#[test]
fn wit_keywords_are_escaped() {
    let text = only_wit(&parsed(&[(
        "x",
        "type A { record: u16, tuple: u16, map: u16, stream: bool }",
    )]));
    for line in [
        "    %record: u16,",
        "    %tuple: u16,",
        "    %map: u16,",
        "    %stream: bool,",
    ] {
        assert!(text.lines().any(|held| held == line), "{line} in {text}");
    }
    parses("keywords", &text);
    let fixture = folder_wit("capgen/fixture");
    assert!(
        fixture.contains("\n  count: func(%flags: list<bool>) -> result<u32, refusal>;\n"),
        "{fixture}"
    );
}

#[test]
fn wit_a_fixed_stem_is_refused() {
    for stem in ["sdk", "plugin", "foo_types"] {
        let error = refused(&[(stem, "type A { a: u16 }")]);
        assert_eq!(error.code, "capgen_reserved_name", "{stem}: {error:?}");
        assert_eq!(error.detail, format!("wit: {stem}"));
        assert_eq!(error.file, format!("{stem}.cap"));
    }
    parses(
        "types",
        &only_wit(&parsed(&[("types", "type A { a: u16 }")])),
    );
}

#[test]
fn wit_a_name_with_no_kebab_form_is_refused() {
    for name in ["a__b", "a_", "a_2"] {
        let error = refused(&[("x", &format!("type A {{ {name}: u16 }}"))]);
        assert_eq!(error.code, "capgen_wit_name", "{name}: {error:?}");
        assert_eq!(error.detail, format!("wit: {name}"));
    }
    let variant = refused(&[("x", "enum E { a_2, b }")]);
    assert_eq!(variant.code, "capgen_wit_name", "{variant:?}");
    let function = refused(&[(
        "x",
        "capability x.go_2(a: u16) -> unit\n  tier: t1\n  needs: x.y\n",
    )]);
    assert_eq!(function.code, "capgen_wit_name", "{function:?}");
}

#[test]
fn wit_a_foreign_name_equal_to_a_local_one_is_aliased() {
    let text = only_wit(&parsed(&[
        ("a", "enum Mode { x }\ntype T { m: b.Mode, here: Mode }"),
        ("b", "enum Mode { z }"),
    ]));
    assert!(
        text.contains("\n  use b-types.{mode as b-mode};\n"),
        "{text}"
    );
    assert!(
        text.contains("\n    m: b-mode,\n    here: mode,\n"),
        "{text}"
    );
    parses("foreign alias", &text);
}

#[test]
fn wit_a_handle_is_a_u64_alias_and_a_foreign_ref_uses_its_types_interface() {
    let text = folder_wit("capgen/fixture_two");
    for held in [
        "\ninterface secrets-types {\n",
        "\n  type secret = u64;\n",
        "\ninterface net-types {\n  use secrets-types.{mode, secret};\n\n",
        "\n    bearer: option<secret>,\n",
        "\n    keys: list<secret>,\n",
        "\n    mode: mode,\n",
        "\nworld plugin {}\n",
    ] {
        assert!(text.contains(held), "{held:?} in {text}");
    }
    assert!(!text.contains("resource"), "{text}");
    assert!(
        !text.contains("use sdk-types."),
        "no capability, no refusal: {text}"
    );
}

#[test]
fn wit_a_types_use_cycle_is_refused() {
    let error = refused(&[
        ("a", "enum M { x }\ntype T { n: b.N }"),
        ("b", "enum N { y }\ntype U { m: a.M }"),
    ]);
    assert_eq!(error.code, "capgen_wit_cycle", "{error:?}");
    let calls = only_wit(&parsed(&[
        (
            "a",
            "enum M { x }\ncapability a.go(n: b.N) -> unit\n  tier: t1\n  needs: a.y\n",
        ),
        ("b", "enum N { y }\ntype U { m: a.M }"),
    ]));
    parses("calls across", &calls);
    let chain = only_wit(&parsed(&[
        ("a", "enum M { x }\ntype T { n: b.N }"),
        ("b", "enum N { y }\ntype U { o: c.O }"),
        ("c", "enum O { z }"),
    ]));
    parses("chain", &chain);
}

#[test]
fn wit_has_no_caller_parameter() {
    let mut texts = vec![only_wit(&parsed(&[("settings", SETTINGS)]))];
    for (folder, _) in FIXTURES {
        texts.push(folder_wit(&format!("capgen/{folder}")));
    }
    for text in &texts {
        for word in ["caller", "host", "plugin"] {
            assert!(!text.contains(&format!("({word}: ")), "{word} in {text}");
            assert!(
                !text
                    .lines()
                    .any(|line| line.trim_start().starts_with(&format!("{word}: "))),
                "{word} in {text}"
            );
        }
    }
    assert!(texts.iter().filter(|text| text.contains("func(")).count() >= 3);
}

#[test]
fn wit_only_name_lines_pass_100_columns() {
    let pascal = format!("A{}", "b".repeat(63));
    let snake = format!("a{}", "_b".repeat(31)) + "c";
    let handle = format!("H{}", "c".repeat(63));
    let variant = format!("v{}", "d".repeat(63));
    let other = format!("w{}", "e".repeat(63));
    let kebab: Vec<String> = [&pascal, &handle]
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .chain(
            [&snake, &variant, &other]
                .iter()
                .map(|name| name.replace('_', "-")),
        )
        .collect();
    assert!(kebab.iter().all(|name| name.len() == 64), "{kebab:?}");
    let source = format!(
        "enum E {{ {variant}, {other} }}\nhandle {handle}\n\
         type {pascal} {{ {snake}: list<option<u64>>, {other}: E, h: {handle} }}\n\
         capability x.go({snake}: {pascal}, {other}: list<bytes>) -> {pascal}\n  tier: t1\n  needs: x.y\n"
    );
    let text = only_wit(&parsed(&[("x", &source)]));
    for line in text.lines() {
        assert!(
            line.len() <= 100 || kebab.iter().any(|name| line.contains(name.as_str())),
            "line over 100 without a long name: {line}"
        );
    }
    assert!(text.contains("  go: func(\n"), "{text}");
    parses("long names", &text);
}

#[test]
fn wit_caps_generate_the_checked_in_file() {
    let loaded = load(&sdk("caps")).expect("caps/ loads");
    let borrowed: Vec<(&str, File)> = loaded
        .iter()
        .map(|(stem, file)| (stem.as_str(), file.clone()))
        .collect();
    let files = wit(&borrowed).expect("caps/ generates");
    assert_eq!(files.len(), 1);
    assert_eq!(crate::drift(&sdk("wit"), &files), Vec::<String>::new());
}

#[test]
fn wit_wire_covers_lists_nested_options_and_a_foreign_alias() {
    let text = folder_wit("capgen/fixture_wire");
    for held in [
        "\ninterface wire {\n  use sdk-types.{refusal};\n  use wire-types.{blob, mode, plain};\n  use modes-types.{mode as modes-mode};\n\n",
        "\n  blobs: func(plain: plain) -> result<list<blob>, refusal>;\n",
        "\n  nested: func(values: list<option<u64>>) -> result<list<option<u64>>, refusal>;\n",
        "\n  swap: func(here: mode, there: modes-mode) -> result<modes-mode, refusal>;\n",
        "\ninterface modes-types {\n",
        "\nworld plugin {\n  import wire;\n}\n",
    ] {
        assert!(text.contains(held), "{held:?} in {text}");
    }
    assert!(!text.contains("\ninterface modes {\n"), "{text}");
    parses("fixture_wire", &text);
}

/// Times 1000 runs of `wit` over the three fixtures and the in-test settings source and
/// checks the p95 against the budget of the `OVERHEAD_MAC` condition (quiet 1 ms, loaded
/// 5 ms). Run it as
/// `OVERHEAD_MAC=quiet cargo test --release -p crazyagent-capgen --lib wit_tests::wit_generation_p95 -- --ignored --nocapture`.
#[test]
#[ignore = "speed reading, run by name with OVERHEAD_MAC set"]
fn wit_generation_p95() {
    let condition = std::env::var("OVERHEAD_MAC").expect("OVERHEAD_MAC is quiet or loaded");
    let budget_us = match condition.as_str() {
        "quiet" => 1_000,
        "loaded" => 5_000,
        other => panic!("OVERHEAD_MAC is quiet or loaded, not {other}"),
    };
    let mut inputs: Vec<Vec<(String, File)>> = FIXTURES
        .iter()
        .map(|(folder, _)| load(&sdk(&format!("capgen/{folder}"))).expect("the fixture loads"))
        .collect();
    inputs.push(vec![(
        "settings".to_owned(),
        crazyagent_cap::parse_as("settings", SETTINGS).expect("parses"),
    )]);
    let inputs: Vec<Vec<(&str, File)>> = inputs
        .iter()
        .map(|files| {
            files
                .iter()
                .map(|(stem, file)| (stem.as_str(), file.clone()))
                .collect()
        })
        .collect();
    let mut samples: Vec<u128> = (0..1000)
        .map(|_| {
            let start = Instant::now();
            for files in &inputs {
                std::hint::black_box(wit(files).expect("generates"));
            }
            start.elapsed().as_micros()
        })
        .collect();
    samples.sort_unstable();
    let p95 = samples[samples.len() * 95 / 100 - 1];
    println!("capgen_wit_p95_us={p95} condition={condition}");
    assert!(
        p95 <= budget_us,
        "p95 {p95} us over {budget_us} us ({condition})"
    );
}
