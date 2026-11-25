use super::*;

#[test]
fn lookups_before_the_start_of_short_words_match_nothing() {
    for word in ["fox", "ox", "x", "aux", "czar", "cz", "llo", "al", "wicz"] {
        let result = double_metaphone(word);
        assert!(!result.primary.is_empty() || word == "x", "{word}");
    }
    assert_eq!(double_metaphone("fox").primary, "FKS");
}

#[test]
fn test_smith_smyth() {
    let smith = double_metaphone("Smith");
    let spelled_smyth = double_metaphone("Smyth");

    assert_eq!(smith.primary, "SM0");
    assert_eq!(smith.alternate.as_deref(), Some("SMT"));

    assert_eq!(spelled_smyth.primary, "SM0");
    assert_eq!(spelled_smyth.alternate.as_deref(), Some("SMT"));

    assert!(
        smith.matches(&spelled_smyth),
        "Smith and Smyth must match in Double Metaphone"
    );
}

#[test]
fn test_pfizespelled_fizer() {
    let pfizer = double_metaphone("Pfizer");
    let spelled_fizer = double_metaphone("Fizer");

    assert_eq!(pfizer.primary, "FSR");
    assert_eq!(spelled_fizer.primary, "FSR");

    assert!(
        pfizer.matches(&spelled_fizer),
        "Pfizer and Fizer must match in Double Metaphone"
    );
}

#[test]
fn test_gerard_guerard() {
    let accented = double_metaphone("Gérard");
    let unaccented = double_metaphone("Guerard");

    assert!(
        accented.matches(&unaccented),
        "Gérard and Guerard must match in Double Metaphone: gerard={accented:?}, guerard={unaccented:?}"
    );
}

#[test]
fn test_accents_and_empty() {
    assert_eq!(double_metaphone("").primary, "");
    assert_eq!(double_metaphone("12345").primary, "");
    assert_eq!(double_metaphone("Catherine").primary, "K0RN");
    assert_eq!(double_metaphone("Katherine").primary, "K0RN");
    assert!(double_metaphone("Catherine").matches(&double_metaphone("Katherine")));
}
