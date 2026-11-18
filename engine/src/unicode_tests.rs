// SPDX-License-Identifier: GPL-3.0-or-later
use crate::unicode::*;
use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use icu_properties::{
    CodePointMapData,
    props::{GeneralCategory as GC, GeneralCategoryGroup as G},
};

fn reference(input: &str) -> String {
    let lower = input.to_lowercase();
    let categories = CodePointMapData::<GC>::new();
    let decomposed = DecomposingNormalizerBorrowed::new_nfd().normalize_iter(lower.chars());
    let stripped = decomposed.filter(|&c| !G::Mark.contains(categories.get(c)));
    ComposingNormalizerBorrowed::new_nfc()
        .normalize_iter(stripped)
        .collect()
}

#[test]
fn matches_pinned_icu_for_every_unicode_scalar_and_category_predicate() {
    let categories = CodePointMapData::<GC>::new();
    let words = G::Letter
        .union(G::Mark)
        .union(G::Number)
        .union(G::ConnectorPunctuation);
    let stem = G::LowercaseLetter
        .union(G::TitlecaseLetter)
        .union(G::ModifierLetter)
        .union(G::OtherLetter);
    for c in (0..=0x10ffff).filter_map(char::from_u32) {
        let g = categories.get(c);
        assert_eq!(
            (is_wordchar(c), is_digit(c), should_stem(c)),
            (words.contains(g), g == GC::DecimalNumber, stem.contains(g)),
            "{c:?}"
        );
        let mut bytes = [0; 4];
        let input = c.encode_utf8(&mut bytes);
        assert_eq!(remove_accents(input), reference(input), "{c:?}");
    }
}

#[test]
fn preserves_context_and_recomposes_across_removed_marks() {
    for s in [
        "Σάββατο ΟΣ ΣΑΣ Ångström İstanbul",
        "\u{16d63}\u{301}\u{16d67}\u{20dd}\u{16d67}",
        "\u{16d67}\u{16d68}\u{16d68}",
        "ᄒ\u{ac00}ᆨ\u{301}ᆫᄂ\u{301}ᅡ",
        "𠀟/䶿/車 カ\u{3099} 가나다라마바사 हिंदी",
        "\u{345}\u{323}\u{034f}\u{300}\u{31b}",
    ] {
        assert_eq!(remove_accents(s), reference(s));
    }
    // Include every algorithmic Hangul composition at a scalar boundary.
    for c in ('\u{ac00}'..='\u{d7a3}').step_by(28) {
        let lv = c as u32 - 0xac00;
        let l = char::from_u32(0x1100 + lv / (21 * 28)).unwrap();
        let v = char::from_u32(0x1161 + (lv / 28) % 21).unwrap();
        for t in '\u{11a8}'..='\u{11c2}' {
            let s = format!("{l}\u{20dd}{v}\u{301}{t}");
            assert_eq!(remove_accents(&s), reference(&s));
        }
    }
}
