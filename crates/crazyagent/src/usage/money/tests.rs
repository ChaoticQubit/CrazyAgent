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
            prices: PriceSource::Bundled
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
