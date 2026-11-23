use crate::permission::Tier;

#[test]
fn a_tier_word_maps_to_its_tier() {
    assert_eq!(Tier::from_word("t1"), Some(Tier::T1));
    assert_eq!(Tier::from_word("t2"), Some(Tier::T2));
    assert_eq!(Tier::from_word("t3"), Some(Tier::T3));
    for word in ["kernel", "t0", "t4", "T1", "", "t1 "] {
        assert_eq!(Tier::from_word(word), None, "{word:?}");
    }
    assert!(Tier::Kernel < Tier::T1 && Tier::T1 < Tier::T2 && Tier::T2 < Tier::T3);
}
