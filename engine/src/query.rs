//! Xapian 1.4's `QueryParser` (`queryparser.lemony`), for the flags libzim
//! uses: `FLAG_CJK_NGRAM` alone (full-text search; the suggestion phrases),
//! and `FLAG_DEFAULT | FLAG_PARTIAL | FLAG_CJK_NGRAM` (suggestions), with the
//! default operator AND and no stopper (libzim's indexes have no stopwords).
//!
//! libzim lowercases every query first (`removeAccents`), so the parts of
//! the grammar that need capitals (acronyms, `AND`/`OR`/`NOT`/`NEAR`…)
//! never fire and aren't ported.
// ponytail: no brackets, field prefixes or boolean keywords; port them from
// the lemony grammar when a real (lowercase) query parses differently.

use crate::text::{Infix, Stemmer, infix, is_digit, is_unbroken, is_wordchar, lower, should_stem};

/// A parsed query (Xapian's `Query` tree, the operators libzim's queries produce).
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    /// A term at a query position.
    Term(String, u32),
    And(Vec<Query>),
    Or(Vec<Query>),
    AndNot(Box<Query>, Box<Query>),
    /// Terms within `window` positions, in order (exact when window = len).
    Phrase(Vec<Query>, u32),
    /// The terms starting with a prefix, weighted as one term (`OP_SYNONYM`);
    /// at most `max` of the most frequent (0: all).
    Wildcard(String, usize),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Stem {
    None,
    /// Stem words starting with a letter, as `Z` + stem.
    Some,
    /// Stem everything, no `Z`.
    All,
}

/// Which parts of the syntax are on.
#[derive(Clone, Copy)]
pub struct Flags {
    /// `FLAG_PHRASE | FLAG_LOVEHATE` (from `FLAG_DEFAULT`): `"…"`, `+word`, `-word`.
    pub syntax: bool,
    /// `FLAG_PARTIAL`: the last word also matches as a prefix.
    pub partial: bool,
}

pub struct Parser<'a> {
    pub stemmer: Option<&'a Stemmer>,
    pub stem: Stem,
    pub flags: Flags,
    /// Whether a term is in the database (for `c++`-style suffixes).
    pub exists: &'a dyn Fn(&str) -> bool,
}

/// Xapian's default `set_max_partial_expansion`.
const PARTIAL_EXPANSION: usize = 100;

fn is_phrase_generator(c: char) -> bool {
    ".-/:\\@".contains(c)
}

fn is_stem_preventer(c: char) -> bool {
    "(/\\@<>=*[{\"".contains(c)
}

fn is_double_quote(c: char) -> bool {
    matches!(c, '"' | '\u{201c}' | '\u{201d}')
}

/// A term before it becomes a query (Xapian's `Term`).
struct Term {
    name: String,
    stem: Stem,
    pos: u32,
    unbroken: bool,
}

enum Item {
    Plain(Query),
    Hate(Query),
}

impl Parser<'_> {
    fn make_term(&self, t: &Term) -> String {
        match (t.stem, self.stemmer) {
            (Stem::None, _) | (_, None) => t.name.clone(),
            (Stem::Some, Some(s)) => format!("Z{}", s.stem(&t.name)),
            (Stem::All, Some(s)) => s.stem(&t.name).into_owned(),
        }
    }

    fn term_query(&self, t: &Term) -> Query {
        if t.unbroken {
            // as_unbroken_query: AND of the n-grams, unstemmed.
            let chars: Vec<char> = t.name.chars().collect();
            let mut v = vec![];
            for i in 0..chars.len() {
                v.push(Query::Term(chars[i].to_string(), t.pos));
                if i + 1 < chars.len() {
                    v.push(Query::Term(chars[i..i + 2].iter().collect(), t.pos));
                }
            }
            return and(v);
        }
        Query::Term(self.make_term(t), t.pos)
    }

    /// A phrase over `terms` (each needs positions: STEM_SOME → none).
    fn phrase(&self, terms: Vec<Term>) -> Query {
        let mut v = vec![];
        for mut t in terms {
            if t.stem == Stem::Some {
                t.stem = Stem::None;
            }
            if t.unbroken {
                // as_positional_unbroken: each character, at the same position.
                for c in t.name.chars() {
                    v.push(Query::Term(
                        self.make_term(&Term {
                            name: c.to_string(),
                            unbroken: false,
                            ..t
                        }),
                        t.pos,
                    ));
                }
            } else {
                v.push(self.term_query(&t));
            }
        }
        let n = v.len() as u32;
        match v.len() {
            0 => Query::And(vec![]),
            1 => v.pop().unwrap(),
            _ => Query::Phrase(v, n),
        }
    }

    /// `parse_term`: one word starting at `s[*i]` (a word character).
    /// Returns (term, needs word break).
    fn parse_term(&self, s: &[char], i: &mut usize) -> (String, bool) {
        let n = s.len();
        if is_unbroken(s[*i]) {
            let start = *i;
            while *i < n && is_unbroken(s[*i]) && is_wordchar(s[*i]) {
                *i += 1;
            }
            return (s[start..*i].iter().collect(), true);
        }
        let mut term = String::new();
        let mut prev = s[*i];
        term.push(prev);
        loop {
            *i += 1;
            if *i == n || is_unbroken(s[*i]) {
                break;
            }
            let mut ch = s[*i];
            if !is_wordchar(ch) {
                let Some(&next) = s.get(*i + 1).filter(|&&c| is_wordchar(c)) else {
                    break;
                };
                match infix(ch, is_digit(prev) && is_digit(next)) {
                    Infix::End => break,
                    Infix::Ignore => continue,
                    Infix::Join(c) => ch = c,
                }
            }
            term.push(ch);
            prev = ch;
        }
        if *i < n && matches!(s[*i], '+' | '#') {
            let mut suff = term.clone();
            let mut p = *i;
            loop {
                if suff.len() - term.len() == 3 {
                    suff.clear();
                    break;
                }
                suff.push(s[p]);
                p += 1;
                if !(p < n && matches!(s[p], '+' | '#')) {
                    break;
                }
            }
            if !suff.is_empty() && (p == n || !is_wordchar(s[p])) {
                let lc = |t: &str| t.chars().map(lower).collect::<String>();
                if (self.exists)(&lc(&suff)) || !(self.exists)(&lc(&term)) {
                    term = suff;
                    *i = p;
                }
            }
        }
        (term, false)
    }

    /// Parse `q`; `None` if it has no terms.
    pub fn parse(&self, q: &str) -> Option<Query> {
        let s: Vec<char> = q.chars().collect();
        let n = s.len();
        let mut i = 0;
        let mut pos = 1u32;
        let mut items: Vec<Item> = vec![];
        let mut newprev = ' ';
        let mut in_quotes: Option<Vec<Term>> = None;
        let mut hate_next = false;
        // A phrased term (`a-b`) being built.
        let mut phrased: Vec<Term> = vec![];
        let push = |items: &mut Vec<Item>, q: Query, hate: &mut bool| {
            items.push(if std::mem::take(hate) {
                Item::Hate(q)
            } else {
                Item::Plain(q)
            });
        };
        'main: while i < n {
            if s[i].is_whitespace() {
                newprev = ' ';
                while i < n && s[i].is_whitespace() {
                    i += 1;
                }
                if i == n {
                    break;
                }
            }
            if !is_wordchar(s[i]) {
                let prev = newprev;
                let ch = s[i];
                i += 1;
                newprev = ch;
                match ch {
                    '"' | '\u{201c}' | '\u{201d}' => {
                        if in_quotes.is_none() {
                            while i < n && s[i].is_whitespace() {
                                i += 1;
                            }
                            if i == n {
                                break 'main;
                            }
                            if is_double_quote(s[i]) {
                                newprev = s[i];
                                i += 1;
                                continue;
                            }
                        }
                        if self.flags.syntax {
                            if ch == '"' && s.get(i) == Some(&'"') {
                                i += 1;
                                continue;
                            }
                            match in_quotes.take() {
                                None => in_quotes = Some(vec![]),
                                Some(terms) => {
                                    if !terms.is_empty() {
                                        let p = self.phrase(terms);
                                        push(&mut items, p, &mut hate_next);
                                    }
                                }
                            }
                        }
                    }
                    '+' | '-' => {
                        if i == n {
                            break 'main;
                        }
                        if prev > ' ' && prev != '(' {
                            continue;
                        }
                        if s[i].is_whitespace() || s[i] == '+' || s[i] == '-' {
                            newprev = s[i];
                            i += 1;
                            continue;
                        }
                        if in_quotes.is_none() && self.flags.syntax {
                            // LOVE: with AND as the default operator, a plain term.
                            hate_next = ch == '-';
                        }
                    }
                    _ => {}
                }
                continue;
            }
            // A word (or the next word of a phrased term).
            loop {
                newprev = 'A';
                let (name, unbroken) = self.parse_term(&s, &mut i);
                let unstemmed = name.clone();
                let name: String = name.chars().map(lower).collect();
                let mut stem = self.stem;
                if self.stemmer.is_none()
                    || stem == Stem::Some
                        && (!should_stem(&unstemmed)
                            || s.get(i).is_some_and(|&c| is_stem_preventer(c)))
                {
                    stem = Stem::None;
                }
                let t = Term {
                    name,
                    stem,
                    pos,
                    unbroken,
                };
                pos += 1;
                if let Some(terms) = in_quotes.as_mut() {
                    terms.push(t);
                    break;
                }
                if unbroken {
                    let q = self.term_query(&t);
                    push(&mut items, q, &mut hate_next);
                    continue 'main;
                }
                if phrased.is_empty() && i == n && self.flags.partial {
                    // as_partial_query: OR(SYNONYM(WILDCARD), the term).
                    let q = Query::Or(vec![
                        Query::Wildcard(t.name.clone(), PARTIAL_EXPANSION),
                        self.term_query(&t),
                    ]);
                    push(&mut items, q, &mut hate_next);
                    continue 'main;
                }
                phrased.push(t);
                // Phrase generators join the next word into a phrase.
                if i < n && is_phrase_generator(s[i]) {
                    let mut p = i;
                    while p < n && is_phrase_generator(s[p]) {
                        p += 1;
                    }
                    if p < n && is_wordchar(s[p]) {
                        i = p;
                        continue;
                    }
                    i = p;
                }
                break;
            }
            match phrased.len() {
                0 => {}
                1 => {
                    let t = phrased.pop().unwrap();
                    let q = self.term_query(&t);
                    push(&mut items, q, &mut hate_next);
                }
                _ => {
                    let q = self.phrase(std::mem::take(&mut phrased));
                    push(&mut items, q, &mut hate_next);
                }
            }
        }
        if let Some(terms) = in_quotes
            && !terms.is_empty()
        {
            let p = self.phrase(terms);
            push(&mut items, p, &mut hate_next);
        }
        let (mut plain, mut hate) = (vec![], vec![]);
        for it in items {
            match it {
                Item::Plain(q) => plain.push(q),
                Item::Hate(q) => hate.push(q),
            }
        }
        if plain.is_empty() {
            return None; // nothing, or only hated terms: a parse failure
        }
        let q = and(plain);
        Some(if hate.is_empty() {
            q
        } else {
            Query::AndNot(Box::new(q), Box::new(or(hate)))
        })
    }
}

fn and(mut v: Vec<Query>) -> Query {
    if v.len() == 1 {
        v.pop().unwrap()
    } else {
        Query::And(v)
    }
}

fn or(mut v: Vec<Query>) -> Query {
    if v.len() == 1 {
        v.pop().unwrap()
    } else {
        Query::Or(v)
    }
}

impl Query {
    /// `get_terms_begin()`: the terms by (position, term), duplicates at
    /// the same position dropped.
    pub fn terms(&self) -> Vec<(u32, String)> {
        fn gather(q: &Query, out: &mut Vec<(u32, String)>) {
            match q {
                Query::Term(t, p) => out.push((*p, t.clone())),
                Query::And(v) | Query::Or(v) | Query::Phrase(v, _) => {
                    v.iter().for_each(|q| gather(q, out))
                }
                Query::AndNot(a, b) => {
                    gather(a, out);
                    gather(b, out);
                }
                Query::Wildcard(..) => {}
            }
        }
        let mut v = vec![];
        gather(self, &mut v);
        v.sort();
        v.dedup();
        v
    }

    /// `get_length()`: the number of term leaves.
    pub fn length(&self) -> u32 {
        match self {
            Query::Term(..) => 1,
            Query::And(v) | Query::Or(v) | Query::Phrase(v, _) => v.iter().map(Query::length).sum(),
            Query::AndNot(a, b) => a.length() + b.length(),
            Query::Wildcard(..) => 0,
        }
    }
}

/// libzim's suggestion query for (unaccented) `q`: `OR(OR(main, phrase),
/// anchored phrase)`, or a wildcard on the whole query when `main` is empty.
pub fn suggestion(
    q: &str,
    stemmer: Option<&Stemmer>,
    exists: &dyn Fn(&str) -> bool,
) -> Option<Query> {
    if q.is_empty() {
        return None;
    }
    let parser = |stem, syntax, partial| Parser {
        stemmer,
        stem,
        flags: Flags { syntax, partial },
        exists,
    };
    let Some(main) = parser(Stem::Some, true, true).parse(q) else {
        return Some(Query::Wildcard(q.to_string(), 0));
    };
    let plain = parser(Stem::None, false, false);
    let phrase = |s: &str| -> Option<Query> {
        let p = plain.parse(s)?;
        let len = p.length();
        let mut terms: Vec<Query> = p
            .terms()
            .into_iter()
            .map(|(pos, t)| Query::Term(t, pos))
            .collect();
        Some(if terms.len() == 1 {
            terms.pop().unwrap()
        } else {
            Query::Phrase(terms, len)
        })
    };
    let mut v = vec![main];
    v.extend(phrase(q));
    let first = Query::Or(v);
    Some(match phrase(&format!("{}{q}", crate::index::ANCHOR)) {
        Some(a) => Query::Or(vec![first, a]),
        None => first,
    })
}

/// libzim's full-text query for (unaccented) `q`: STEM_ALL, AND.
pub fn fulltext(
    q: &str,
    stemmer: Option<&Stemmer>,
    exists: &dyn Fn(&str) -> bool,
) -> Option<Query> {
    Parser {
        stemmer,
        stem: Stem::All,
        flags: Flags {
            syntax: false,
            partial: false,
        },
        exists,
    }
    .parse(q)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::stemmer;
    use Query::*;

    fn t(s: &str, p: u32) -> Query {
        Term(s.into(), p)
    }

    #[test]
    fn fulltext_queries() {
        let en = stemmer("eng");
        let no = |_: &str| false;
        let f = |q: &str| fulltext(q, en.as_ref(), &no);
        assert_eq!(
            f("climate changes"),
            Some(And(vec![t("climat", 1), t("chang", 2)]))
        );
        assert_eq!(
            f("co2-emissions"),
            Some(Phrase(vec![t("co2", 1), t("emiss", 2)], 2))
        );
        assert_eq!(
            f("\"a\" +b -c"),
            Some(And(vec![t("a", 1), t("b", 2), t("c", 3)]))
        );
        assert_eq!(f("  ,;  "), None);
        assert_eq!(
            f("平方"),
            Some(And(vec![t("平", 1), t("平方", 1), t("方", 1)]))
        );
    }

    #[test]
    fn suggestion_queries() {
        let en = stemmer("eng");
        let no = |_: &str| false;
        let p = |q: &str| {
            Parser {
                stemmer: en.as_ref(),
                stem: Stem::Some,
                flags: Flags {
                    syntax: true,
                    partial: true,
                },
                exists: &no,
            }
            .parse(q)
        };
        assert_eq!(
            p("berlin wall"),
            Some(And(vec![
                t("Zberlin", 1),
                Or(vec![Wildcard("wall".into(), 100), t("Zwall", 2)])
            ]))
        );
        assert_eq!(p("wall "), Some(t("Zwall", 1)));
        assert_eq!(
            p("\"hour of\""),
            Some(Phrase(vec![t("hour", 1), t("of", 2)], 2))
        );
        assert_eq!(
            p("berlin -wall x"),
            Some(AndNot(
                Box::new(And(vec![
                    t("Zberlin", 1),
                    Or(vec![Wildcard("x".into(), 100), t("Zx", 3)])
                ])),
                Box::new(t("Zwall", 2))
            ))
        );
        assert_eq!(p("-wall"), None);
        assert_eq!(
            p("2 a"),
            Some(And(vec![
                t("2", 1),
                Or(vec![Wildcard("a".into(), 100), t("Za", 2)])
            ]))
        );
        let s = suggestion("wo", en.as_ref(), &no).unwrap();
        assert_eq!(
            s,
            Or(vec![
                Or(vec![
                    Or(vec![Wildcard("wo".into(), 100), t("Zwo", 1)]),
                    t("wo", 1)
                ]),
                Phrase(vec![t("0posanchor", 1), t("wo", 2)], 2)
            ])
        );
        assert_eq!(
            suggestion("&", en.as_ref(), &no),
            Some(Wildcard("&".into(), 0))
        );
    }
}
