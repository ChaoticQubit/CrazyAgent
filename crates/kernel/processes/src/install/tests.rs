use super::*;

fn npm(package: &str, flags: &[&str]) -> InstallRecipe {
    InstallRecipe::NpmGlobal {
        package: package.to_owned(),
        version: "1.2.3".to_owned(),
        verb: Verb::Install,
        flags: flags.iter().map(|flag| (*flag).to_owned()).collect(),
    }
}

fn uv_tool(package: &str, pinned: &str, wanted: &str, lock: usize) -> InstallRecipe {
    InstallRecipe::UvTool {
        package: package.to_owned(),
        version: pinned.to_owned(),
        python: wanted.to_owned(),
        lock: "x".repeat(lock),
    }
}

fn release(pinned: &str, triple: &str, hash: &str) -> InstallRecipe {
    InstallRecipe::UvRelease {
        version: pinned.to_owned(),
        target: triple.to_owned(),
        sha256: hash.to_owned(),
    }
}

fn update(args: &[&str]) -> InstallRecipe {
    InstallRecipe::SelfUpdate {
        program: "codex".to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
    }
}

const HASH: &str = "743f85035a1996ecb350659e01cd9faea9709829be09b1b4b03c73eaf3c9cd73";

/// A package name carrying a second argument is refused as a name, and every field that
/// starts with `-` is refused, so no field adds an option.
#[test]
fn a_recipe_with_an_extra_argument_is_refused() {
    assert_eq!(
        validate(&uv_tool("graphifyy --index-url=http://x", "1.0", "3.12", 10)),
        Err(NAME_INVALID)
    );
    assert_eq!(validate(&uv_tool("-graphifyy", "1.0", "3.12", 10)), Err(NAME_INVALID));
    assert_eq!(validate(&uv_tool("graphifyy", "-1.0", "3.12", 10)), Err(VERSION_INVALID));
    assert_eq!(validate(&uv_tool("graphifyy", "1.0", "-3.12", 10)), Err(TARGET_INVALID));
    assert_eq!(validate(&npm("-codex", &[])), Err(NAME_INVALID));
    assert_eq!(validate(&release("-1", "aarch64-apple-darwin", HASH)), Err(VERSION_INVALID));
    assert_eq!(validate(&release("1", "-x", HASH)), Err(TARGET_INVALID));
    let brew = InstallRecipe::BrewFormula {
        name: "-tmux".to_owned(),
        verb: Verb::Install,
    };
    assert_eq!(validate(&brew), Err(NAME_INVALID));
    let winget = InstallRecipe::Winget {
        id: "-astral-sh.uv".to_owned(),
        version: "0.9.0".to_owned(),
    };
    assert_eq!(validate(&winget), Err(NAME_INVALID));
    let system = InstallRecipe::SystemPackage {
        manager: SystemManager::AptGet,
        package: "-tmux".to_owned(),
    };
    assert_eq!(validate(&system), Err(NAME_INVALID));
    assert_eq!(validate(&update(&["-update"])), Err(TEMPLATE_MISMATCH));
}

/// A self-update whose arguments are well-formed words but not a bundled list is refused.
#[test]
fn an_unlisted_self_update_arg_list_is_refused() {
    assert_eq!(validate(&update(&["update"])), Ok(()));
    assert_eq!(validate(&update(&["upgrade"])), Err(TEMPLATE_MISMATCH));
    assert_eq!(validate(&update(&["update", "now"])), Err(TEMPLATE_MISMATCH));
}

/// A well-shaped npm flag not on the bundled list is refused as unlisted.
#[test]
fn an_unlisted_npm_flag_is_refused() {
    assert_eq!(validate(&npm("@openai/codex", &["--ignore-scripts"])), Ok(()));
    assert_eq!(
        validate(&npm("@openai/codex", &["--foreground-scripts"])),
        Err(FLAG_UNLISTED)
    );
    assert_eq!(validate(&npm("@openai/codex", &["-x"])), Err(TEMPLATE_MISMATCH));
}

/// A name of `NAME_BYTES_MAX` bytes passes and one byte more is refused.
#[test]
fn a_name_past_its_bound_is_refused() {
    let at = "a".repeat(NAME_BYTES_MAX);
    let past = "a".repeat(NAME_BYTES_MAX + 1);
    assert_eq!(validate(&uv_tool(&at, "1.0", "3.12", 10)), Ok(()));
    assert_eq!(validate(&uv_tool(&past, "1.0", "3.12", 10)), Err(NAME_INVALID));
}

/// A version of `VERSION_BYTES_MAX` bytes passes and one byte more is refused.
#[test]
fn a_version_past_its_bound_is_refused() {
    let at = "1".repeat(VERSION_BYTES_MAX);
    let past = "1".repeat(VERSION_BYTES_MAX + 1);
    assert_eq!(validate(&uv_tool("graphifyy", &at, "3.12", 10)), Ok(()));
    assert_eq!(validate(&uv_tool("graphifyy", &past, "3.12", 10)), Err(VERSION_INVALID));
}

/// A lock of `LOCK_BYTES_MAX` bytes passes and one byte more is refused.
#[test]
fn a_lock_past_its_bound_is_refused() {
    assert_eq!(validate(&uv_tool("graphifyy", "1.0", "3.12", LOCK_BYTES_MAX)), Ok(()));
    assert_eq!(
        validate(&uv_tool("graphifyy", "1.0", "3.12", LOCK_BYTES_MAX + 1)),
        Err(LOCK_TOO_LARGE)
    );
}

/// A SHA-256 must be exactly 64 lower-case hex characters.
#[test]
fn a_hash_not_64_lower_hex_is_refused() {
    let triple = "aarch64-apple-darwin";
    assert_eq!(validate(&release("1", triple, HASH)), Ok(()));
    assert_eq!(validate(&release("1", triple, &HASH[1..])), Err(HASH_INVALID));
    assert_eq!(validate(&release("1", triple, &format!("{HASH}0"))), Err(HASH_INVALID));
    assert_eq!(
        validate(&release("1", triple, &HASH.to_ascii_uppercase())),
        Err(HASH_INVALID)
    );
}

/// A target past `TARGET_BYTES_MAX` bytes, or one no setup download names, is refused.
#[test]
fn a_target_past_its_bound_or_unnamed_is_refused() {
    assert_eq!(validate(&release("1", &"a".repeat(TARGET_BYTES_MAX + 1), HASH)), Err(TARGET_INVALID));
    assert_eq!(validate(&release("1", "riscv64-unknown-linux-gnu", HASH)), Err(TARGET_INVALID));
    for triple in TARGETS {
        assert!(triple.len() <= TARGET_BYTES_MAX);
        assert_eq!(validate(&release("1", triple, HASH)), Ok(()));
    }
}

/// A Python version of `PYTHON_BYTES_MAX` bytes passes and one byte more is refused.
#[test]
fn a_python_past_its_bound_is_refused() {
    let at = "3".repeat(PYTHON_BYTES_MAX);
    let past = "3".repeat(PYTHON_BYTES_MAX + 1);
    assert_eq!(validate(&uv_tool("graphifyy", "1.0", &at, 10)), Ok(()));
    assert_eq!(validate(&uv_tool("graphifyy", "1.0", &past, 10)), Err(TARGET_INVALID));
}

/// A self-update of more than `ARGS_MAX` arguments is refused, and every bundled list fits.
#[test]
fn self_update_args_past_their_bound_are_refused() {
    let many = vec!["update"; ARGS_MAX + 1];
    assert_eq!(validate(&update(&many)), Err(TEMPLATE_MISMATCH));
    for allowed in SELF_UPDATE_ARGS_ALLOWED {
        assert!(!allowed.is_empty() && allowed.len() <= ARGS_MAX);
        assert!(allowed.iter().all(|arg| word(arg)));
    }
}

/// An npm install of more than `NPM_FLAGS_MAX` flags is refused, and every bundled flag
/// passes the option check.
#[test]
fn npm_flags_past_their_bound_are_refused() {
    let many = vec!["--ignore-scripts"; NPM_FLAGS_MAX + 1];
    assert_eq!(validate(&npm("pi", &many)), Err(TEMPLATE_MISMATCH));
    for flag in NPM_FLAGS_ALLOWED {
        assert!(option(flag), "{flag}");
    }
}
