//! Text the way libzim and Xapian 1.4 see it: `removeAccents` (libzim's
//! `tools.cpp`), Unicode character classes and the term generator's word
//! splitting (`termgenerator_internal.cc`, `word-breaker.cc`), and the
//! language → stemmer mapping (libzim passes ICU's 2-letter code to
//! `Xapian::Stem`).

use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use icu_properties::CodePointMapData;
use icu_properties::props::{GeneralCategory as GC, GeneralCategoryGroup as G};

/// libzim's `removeAccents`: the ICU transform `Lower; NFD; [:M:] remove; NFC`.
pub fn remove_accents(s: &str) -> String {
    let lower = s.to_lowercase();
    let nfd = DecomposingNormalizerBorrowed::new_nfd().normalize(&lower);
    let stripped: String = nfd
        .chars()
        .filter(|&c| !G::Mark.contains(category(c)))
        .collect();
    ComposingNormalizerBorrowed::new_nfc()
        .normalize(&stripped)
        .into_owned()
}

pub fn category(c: char) -> GC {
    CodePointMapData::<GC>::new().get(c)
}

/// `Unicode::is_wordchar`: letters, marks, numbers, connector punctuation.
pub fn is_wordchar(c: char) -> bool {
    let g = G::Letter
        .union(G::Mark)
        .union(G::Number)
        .union(G::ConnectorPunctuation);
    g.contains(category(c))
}

/// `Unicode::tolower`: the simple lowercase mapping.
pub fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// `check_wordchar`: a word character, lowercased.
fn wordchar(c: char) -> Option<char> {
    is_wordchar(c).then(|| lower(c))
}

pub fn is_digit(c: char) -> bool {
    category(c) == GC::DecimalNumber
}

/// `is_unbroken_script`: the CJK ranges that are split into n-grams.
pub fn is_unbroken(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x2EFF | 0x3000..=0x9FFF | 0xA700..=0xA71F | 0xAC00..=0xD7AF
        | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF
        | 0x20000..=0x2A6DF | 0x2F800..=0x2FA1F)
}

/// What an infix character does inside a word: joins (as this char), is
/// ignored, or ends the word.
#[derive(PartialEq)]
pub enum Infix {
    Join(char),
    Ignore,
    End,
}

/// `check_infix`, or `check_infix_digit` between two digits.
pub fn infix(c: char, between_digits: bool) -> Infix {
    if matches!(c as u32, 0x200b..=0x200d | 0x2060 | 0xfeff) {
        return Infix::Ignore;
    }
    if between_digits {
        return match c as u32 {
            0x2c | 0x2e | 0x3b | 0x037e | 0x0589 | 0x060d | 0x07f8 | 0x2044 | 0xfe10 | 0xfe13
            | 0xfe14 => Infix::Join(c),
            _ => Infix::End,
        };
    }
    match c {
        '\'' | '&' | '\u{b7}' | '\u{5f4}' | '\u{2027}' => Infix::Join(c),
        '\u{2019}' | '\u{201b}' => Infix::Join('\''),
        _ => Infix::End,
    }
}

/// `should_stem`: the term starts with a (non-uppercase) letter.
pub fn should_stem(term: &str) -> bool {
    let g = G::LowercaseLetter
        .union(G::TitlecaseLetter)
        .union(G::ModifierLetter)
        .union(G::OtherLetter);
    term.chars().next().is_some_and(|c| g.contains(category(c)))
}

/// Xapian's `parse_terms`: split `text` into terms, calling
/// `f(term, positional)`. CJK runs become unigrams (positional) and bigrams
/// (not) when `cjk`. Stops early if `f` returns false.
pub fn parse_terms(
    text: &str,
    cjk: bool,
    with_positions: bool,
    mut f: impl FnMut(&str, bool) -> bool,
) {
    let s: Vec<char> = text.chars().collect();
    let n = s.len();
    let mut i = 0;
    let at = |i: usize| s.get(i).copied();
    'outer: loop {
        // Skip to a word character.
        let mut ch = loop {
            let Some(c) = at(i) else { return };
            if let Some(w) = wordchar(c) {
                break w;
            }
            i += 1;
        };
        let mut term = String::new();
        // Acronyms: U.S.A. → usa.
        if s[i].is_ascii_uppercase() {
            let mut p = i;
            loop {
                term.push(lower(s[p]));
                p += 1;
                if !(at(p) == Some('.')
                    && {
                        p += 1;
                        p < n
                    }
                    && s[p].is_ascii_uppercase())
                {
                    break;
                }
            }
            if term.chars().count() > 1 && at(p).is_none_or(|c| !is_wordchar(c)) {
                i = p;
                if !f(&term, with_positions) {
                    return;
                }
                continue 'outer;
            }
            term.clear();
        }
        loop {
            if cjk && is_unbroken(s[i]) && is_wordchar(s[i]) {
                // Unigrams and bigrams of the run.
                let start = i;
                while i < n && is_unbroken(s[i]) && is_wordchar(s[i]) {
                    i += 1;
                }
                for j in start..i {
                    let uni: String = s[j..j + 1].iter().collect();
                    if !f(&uni, with_positions) {
                        return;
                    }
                    if j + 1 < i {
                        let bi: String = s[j..j + 2].iter().collect();
                        if !f(&bi, false) {
                            return;
                        }
                    }
                }
                ch = loop {
                    let Some(c) = at(i) else { return };
                    if let Some(w) = wordchar(c) {
                        break w;
                    }
                    i += 1;
                };
                continue;
            }
            let mut prev;
            loop {
                term.push(ch);
                prev = ch;
                i += 1;
                if i == n || (cjk && is_unbroken(s[i])) {
                    if !f(&term, with_positions) {
                        return;
                    }
                    continue 'outer;
                }
                match wordchar(s[i]) {
                    Some(w) => ch = w,
                    None => break,
                }
            }
            // s[i] isn't a word character: an infix between two word characters?
            let Some(next) = at(i + 1).and_then(wordchar) else {
                break;
            };
            let digits = is_digit(prev) && is_digit(s[i + 1]);
            match infix(s[i], digits) {
                Infix::End => break,
                Infix::Ignore => {}
                Infix::Join(c) => term.push(c),
            }
            ch = next;
            i += 1;
        }
        // Up to three trailing `+` or `#` (C++, C#), unless a word character follows.
        let len = term.len();
        let mut count = 0;
        while let Some(c @ ('+' | '#')) = at(i) {
            count += 1;
            if count > 3 {
                term.truncate(len);
                break;
            }
            term.push(c);
            i += 1;
            if i == n {
                break;
            }
        }
        if i < n && is_wordchar(s[i]) {
            term.truncate(len);
        }
        if !f(&term, with_positions) {
            return;
        }
    }
}

/// A Snowball stemmer as Xapian 1.4 has it: `rust-stemmers` is Snowball
/// 2017, which matches Xapian 1.4 (`tests/golden/xapian/README.md`).
pub type Stemmer = rust_stemmers::Stemmer;

/// The stemmer for an ISO 639-3 (or 639-1) language code, as libzim picks
/// it (`Locale(lang).getLanguage()`, then `Xapian::Stem`); `None` when
/// Xapian 1.4 has none.
// ponytail: Armenian, Basque, Catalan, Indonesian, Irish, Lithuanian and
// Nepali have Xapian 1.4 stemmers but none here, so they aren't stemmed and
// match Kiwix's stemmed indexes poorly; generate Rust stemmers from Xapian
// 1.4's Snowball sources if those languages matter.
pub fn stemmer(lang: &str) -> Option<Stemmer> {
    use rust_stemmers::Algorithm as R;
    let a = match lang.trim().to_ascii_lowercase().as_str() {
        "en" | "eng" => R::English,
        "fr" | "fra" | "fre" => R::French,
        "de" | "deu" | "ger" => R::German,
        "es" | "spa" => R::Spanish,
        "it" | "ita" => R::Italian,
        "pt" | "por" => R::Portuguese,
        "ru" | "rus" => R::Russian,
        "nl" | "nld" | "dut" => R::Dutch,
        "sv" | "swe" => R::Swedish,
        "nb" | "nn" | "no" | "nob" | "nno" | "nor" => R::Norwegian,
        "da" | "dan" => R::Danish,
        "fi" | "fin" => R::Finnish,
        "hu" | "hun" => R::Hungarian,
        "ro" | "ron" | "rum" => R::Romanian,
        "tr" | "tur" => R::Turkish,
        "ar" | "ara" => R::Arabic,
        "ta" | "tam" => R::Tamil,
        _ => return None,
    };
    Some(Stemmer::create(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(s: &str) -> Vec<(String, bool)> {
        let mut v = vec![];
        parse_terms(s, true, true, |t, p| {
            v.push((t.to_string(), p));
            true
        });
        v
    }

    fn words(s: &str) -> Vec<String> {
        terms(s).into_iter().map(|t| t.0).collect()
    }

    #[test]
    fn accents() {
        assert_eq!(remove_accents("Bêřlïñ Éte"), "berlin ete");
        assert_eq!(remove_accents("Ångström ΣΑΣ"), "angstrom σας");
    }

    #[test]
    fn splitting() {
        assert_eq!(words("Hello, World!"), ["hello", "world"]);
        assert_eq!(
            words("U.S.A. and U.S. are N.A.S.A"),
            ["usa", "and", "us", "are", "nasa"]
        );
        assert_eq!(words("A.b"), ["a", "b"]);
        assert_eq!(words("don't rock&roll l·l"), ["don't", "rock&roll", "l·l"]);
        assert_eq!(words("it\u{2019}s"), ["it's"]);
        assert_eq!(words("1,000.5 a,b"), ["1,000.5", "a", "b"]);
        assert_eq!(words("C++ C# c+++x a++++"), ["c++", "c#", "c", "x", "a"]);
        assert_eq!(words("zero\u{200b}width"), ["zerowidth"]);
        assert_eq!(words("snake_case"), ["snake_case"]);
        assert_eq!(words("état"), ["état"]);
    }

    #[test]
    fn cjk() {
        let t = terms("平方根 abc平方");
        let want = [
            ("平", true),
            ("平方", false),
            ("方", true),
            ("方根", false),
            ("根", true),
            ("abc", true),
            ("平", true),
            ("平方", false),
            ("方", true),
        ];
        let want: Vec<_> = want.iter().map(|(a, b)| (a.to_string(), *b)).collect();
        assert_eq!(t, want);
    }

    #[test]
    fn stemmers() {
        let en = stemmer("eng").unwrap();
        assert_eq!(en.stem("international"), "intern");
        assert_eq!(en.stem("climate"), "climat");
        assert!(stemmer("xyz").is_none());
        assert!(stemmer("fra").is_some());
    }
}
