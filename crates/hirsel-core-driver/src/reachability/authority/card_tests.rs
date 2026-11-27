use super::*;

#[test]
fn grant_ordinals_name_the_same_authority_in_carried_and_reached() {
    for (ordinal, name) in GRANT_NAMES.iter().enumerate() {
        assert_eq!(carried_names(1 << ordinal), [*name]);
        assert_eq!(names(1 << ordinal), [*name]);
    }
    assert_eq!(carried_names(1 << 31), ["function"]);
}
