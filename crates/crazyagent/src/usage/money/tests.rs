use super::*;

const COLUMNS: &str = "INSERT INTO usage_tokens(hour_ms,kind,project,pool,account,agent,harness,session,model,input,output,cache_read,cache_write,cost_nanos) VALUES ";

fn store() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    (root, db)
}

#[test]
fn the_defaults_are_dollars_and_the_bundled_prices() {
    let (_root, db) = store();
    assert_eq!(
        read(&db).unwrap(),
        MoneySettings {
            main_number: MainNumber::Dollars,
            prices: PriceSource::Bundled,
            project_limits: ProjectLimits::Hidden,
        }
    );
}

#[test]
fn every_option_is_stored_and_read_back() {
    let (_root, db) = store();
    for main_number in [MainNumber::Tokens, MainNumber::Dollars] {
        for prices in [PriceSource::Off, PriceSource::Bundled] {
            let settings = MoneySettings {
                main_number,
                prices,
                ..MoneySettings::default()
            };
            write(&db, settings).unwrap();
            assert_eq!(read(&db).unwrap(), settings);
        }
    }
}

#[test]
fn a_choice_survives_reopening_the_store() {
    let root = tempfile::tempdir().unwrap();
    let settings = MoneySettings {
        main_number: MainNumber::Tokens,
        prices: PriceSource::Off,
        ..MoneySettings::default()
    };
    write(&crate::usage::open(root.path()).unwrap(), settings).unwrap();
    assert_eq!(
        read(&crate::usage::open(root.path()).unwrap()).unwrap(),
        settings
    );
}

#[test]
fn with_prices_off_the_view_hides_dollars_and_shows_tokens() {
    let (_root, db) = store();
    db.execute_batch(&format!(
        "{COLUMNS}(3600000,'agent','alpha','','a','t','codex','s','gpt-5',100,0,0,0,900),
         (3600000,'agent','alpha','','a','t','codex','s2','local',7,0,0,0,0)"
    ))
    .unwrap();
    assert_eq!(view(&db, None, 0).unwrap().cost_nanos, 900);
    write(
        &db,
        MoneySettings {
            main_number: MainNumber::Dollars,
            prices: PriceSource::Off,
            ..MoneySettings::default()
        },
    )
    .unwrap();
    let shown = view(&db, None, 0).unwrap();
    assert_eq!(
        (
            shown.shown,
            shown.cost_nanos,
            shown.as_of,
            shown.unpriced_tokens
        ),
        (MainNumber::Tokens, 0, None, 0)
    );
}

#[test]
fn with_bundled_prices_the_view_keeps_the_choice_the_date_and_the_range() {
    let (_root, db) = store();
    db.execute_batch(&format!(
        "{COLUMNS}(3600000,'agent','alpha','','a','t','codex','s1','gpt-5',100,0,0,0,900),
         (3600000,'agent','alpha','','a','t','codex','s2','local',7,0,0,0,0),
         (3600000,'agent','beta','','a','t','codex','s3','gpt-5',100,0,0,0,40),
         (0,'agent','alpha','','a','t','codex','s4','gpt-5',100,0,0,0,2)"
    ))
    .unwrap();
    let settings = MoneySettings {
        main_number: MainNumber::Tokens,
        prices: PriceSource::Bundled,
        ..MoneySettings::default()
    };
    write(&db, settings).unwrap();
    assert_eq!(
        view(&db, Some("alpha"), 1).unwrap(),
        MoneyView {
            settings,
            shown: MainNumber::Tokens,
            as_of: Some(prices::table().as_of().to_owned()),
            cost_nanos: 900,
            unpriced_tokens: 7,
        }
    );
    write(&db, MoneySettings::default()).unwrap();
    let every = view(&db, None, i64::MIN).unwrap();
    assert_eq!((every.shown, every.cost_nanos), (MainNumber::Dollars, 942));
}

#[test]
fn stored_money_settings_without_project_limits_read_as_hidden() {
    let settings: MoneySettings =
        serde_json::from_str(r#"{"main_number":"tokens","prices":"off"}"#).unwrap();
    assert_eq!(settings.project_limits, ProjectLimits::Hidden);
}

#[test]
fn project_limits_travel_as_hidden_and_shown() {
    let shown = MoneySettings {
        project_limits: ProjectLimits::Shown,
        ..MoneySettings::default()
    };
    assert_eq!(
        serde_json::to_string(&shown).unwrap(),
        r#"{"main_number":"dollars","prices":"bundled","project_limits":"shown"}"#
    );
    assert_eq!(
        serde_json::to_string(&MoneySettings::default()).unwrap(),
        r#"{"main_number":"dollars","prices":"bundled","project_limits":"hidden"}"#
    );
}

#[test]
fn project_limits_shown_is_stored_and_survives_reopening_the_store() {
    let root = tempfile::tempdir().unwrap();
    let settings = MoneySettings {
        project_limits: ProjectLimits::Shown,
        ..MoneySettings::default()
    };
    write(&crate::usage::open(root.path()).unwrap(), settings).unwrap();
    assert_eq!(
        read(&crate::usage::open(root.path()).unwrap()).unwrap(),
        settings
    );
}

#[test]
fn project_limits_hidden_is_stored_over_shown() {
    let (_root, db) = store();
    let shown = MoneySettings {
        project_limits: ProjectLimits::Shown,
        ..MoneySettings::default()
    };
    write(&db, shown).unwrap();
    write(&db, MoneySettings::default()).unwrap();
    assert_eq!(read(&db).unwrap().project_limits, ProjectLimits::Hidden);
}

#[test]
fn a_store_made_before_project_limits_keeps_its_choices_and_reads_hidden() {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    db.execute_batch(
        "DROP TABLE usage_project_limits;
         INSERT INTO usage_money (singleton,main_number,prices) VALUES (1,'tokens','off')",
    )
    .unwrap();
    drop(db);
    let reopened = crate::usage::open(root.path()).unwrap();
    assert_eq!(
        read(&reopened).unwrap(),
        MoneySettings {
            main_number: MainNumber::Tokens,
            prices: PriceSource::Off,
            project_limits: ProjectLimits::Hidden,
        }
    );
    let shown = MoneySettings {
        project_limits: ProjectLimits::Shown,
        ..read(&reopened).unwrap()
    };
    write(&reopened, shown).unwrap();
    assert_eq!(read(&reopened).unwrap(), shown);
}

#[test]
fn a_write_that_cannot_store_project_limits_changes_nothing() {
    let (_root, db) = store();
    let before = MoneySettings {
        main_number: MainNumber::Tokens,
        prices: PriceSource::Off,
        project_limits: ProjectLimits::Hidden,
    };
    write(&db, before).unwrap();
    db.execute_batch("DROP TABLE usage_project_limits").unwrap();
    let refused = write(
        &db,
        MoneySettings {
            main_number: MainNumber::Dollars,
            prices: PriceSource::Bundled,
            project_limits: ProjectLimits::Shown,
        },
    );
    assert!(refused.is_err());
    let kept: (String, String) = db
        .query_row("SELECT main_number,prices FROM usage_money", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(kept, ("tokens".to_owned(), "off".to_owned()));
}
