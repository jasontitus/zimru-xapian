//! The documents libzim puts in its two indexes (`xapianIndexer.cpp`,
//! `xapianWorker.cpp`): terms, wdfs and positions as Xapian 1.4's term
//! generator makes them.

use crate::glass::write::{Builder, Terms};
use crate::html;
use crate::text::{Stemmer, parse_terms, remove_accents, should_stem};

/// libzim's `ANCHOR_TERM`: every title starts with it, so a phrase can be
/// anchored to the start.
pub const ANCHOR: &str = "0posanchor ";
/// libzim's `MAX_INDEXABLE_TITLE_WORD_SIZE`.
pub const MAX_TITLE_WORD: usize = 240;
/// Xapian's default maximum word length.
pub const MAX_WORD: usize = 64;

fn add(terms: &mut Terms, term: &str, wdf: u32, pos: Option<u32>) {
    let e = terms.entry(term.as_bytes().to_vec()).or_default();
    e.0 += wdf;
    if let Some(p) = pos {
        e.1.push(p);
    }
}

/// A title document's terms: `"0posanchor " + removeAccents(title)` with
/// positions, STEM_SOME (the word at its position, `Z` + stem without),
/// CJK n-grams. A title of no words is indexed as one whole term instead.
pub fn title_terms(title: &str, stem: Option<&Stemmer>) -> Terms {
    let mut terms = Terms::new();
    let unaccented = remove_accents(title);
    if unaccented.is_empty() {
        return terms;
    }
    let mut pos = 0;
    parse_terms(
        &format!("{ANCHOR}{unaccented}"),
        true,
        true,
        |t, positional| {
            if t.len() > MAX_TITLE_WORD {
                return true;
            }
            if positional {
                pos += 1;
                add(&mut terms, t, 1, Some(pos));
            } else {
                add(&mut terms, t, 1, None);
            }
            if let Some(s) = stem
                && should_stem(t)
            {
                let st = s.stem(t);
                if !st.is_empty() {
                    add(&mut terms, &format!("Z{st}"), 1, None);
                }
            }
            true
        },
    );
    if terms.len() == 1 {
        terms.clear();
        if unaccented.len() <= MAX_TITLE_WORD {
            add(&mut terms, &unaccented, 1, None);
        }
    }
    terms
}

/// Index `text` without positions, STEM_ALL (stems only) with a stemmer,
/// else the words themselves; each occurrence adds `wdf`.
fn index_text(terms: &mut Terms, text: &str, wdf: u32, stem: Option<&Stemmer>) {
    parse_terms(text, true, false, |t, _| {
        if t.len() > MAX_WORD {
            return true;
        }
        match stem {
            Some(s) => {
                let st = s.stem(t);
                if !st.is_empty() {
                    add(terms, &st, wdf, None);
                }
            }
            None => add(terms, t, wdf, None),
        }
        true
    });
}

/// A full-text document's terms: the unaccented content (wdf 1 each), title
/// (`content bytes / 500 + 1` each) and keywords (3 each).
pub fn fulltext_terms(content: &str, title: &str, keywords: &str, stem: Option<&Stemmer>) -> Terms {
    let mut terms = Terms::new();
    index_text(&mut terms, content, 1, stem);
    index_text(&mut terms, title, content.len() as u32 / 500 + 1, stem);
    index_text(&mut terms, keywords, 3, stem);
    terms
}

/// Add an HTML item to the full-text index, as libzim does (skipped if it
/// has no text, a robots `noindex`, or `NOINDEX` in its text). Values:
/// 0 = the unaccented title, 1 = the word count, 2 = the geo position.
pub fn add_fulltext(b: &mut Builder, path: &str, title: &str, html: &str, stem: Option<&Stemmer>) {
    let h = html::parse(html);
    if h.dump.is_empty() || !h.indexing_allowed || h.dump.contains("NOINDEX") {
        return;
    }
    let content = remove_accents(&h.dump);
    let title = remove_accents(title);
    let terms = fulltext_terms(&content, &title, &remove_accents(&h.keywords), stem);
    let mut values = vec![
        (0, title.into_bytes()),
        (1, count_words(&h.dump).to_string().into_bytes()),
    ];
    if let Some(g) = h.geo.and_then(|(lat, lon)| geo(lat, lon)) {
        values.push((2, g));
    }
    b.add(format!("C/{path}").as_bytes(), &terms, &values);
}

/// Add a front article (or redirect) to the title index: values 0 = the
/// title, 1 = the redirect's target path, or its own path.
pub fn add_title(
    b: &mut Builder,
    path: &str,
    title: &str,
    target: Option<&str>,
    stem: Option<&Stemmer>,
) {
    let values = [
        (0, title.as_bytes().to_vec()),
        (1, target.unwrap_or(path).as_bytes().to_vec()),
    ];
    b.add(
        format!("C/{path}").as_bytes(),
        &title_terms(title, stem),
        &values,
    );
}

/// libzim's index metadata.
pub fn set_metadata(b: &mut Builder, fulltext: bool, language: &str) {
    if fulltext {
        b.set_metadata("valuesmap", b"title:0;wordcount:1;geo.position:2");
        b.set_metadata("kind", b"fulltext");
    } else {
        b.set_metadata("valuesmap", b"title:0;targetPath:1");
        b.set_metadata("kind", b"title");
    }
    b.set_metadata("data", b"fullPath");
    b.set_metadata("language", language.as_bytes());
}

/// Xapian's `LatLongCoord::serialise` (`GeoEncode::encode`): 6 bytes.
pub fn geo(lat: f64, lon: f64) -> Option<Vec<u8>> {
    if !(-90.0..=90.0).contains(&lat) {
        return None;
    }
    let mut lon = lon % 360.0;
    if lon < 0.0 {
        lon += 360.0;
    }
    let lat16 = ((lat + 90.0) * 57600.0).round() as i64;
    let lon16 = if lat16 == 0 || lat16 == 57600 * 180 {
        0
    } else {
        match (lon * 57600.0).round() as i64 {
            x if x == 57600 * 360 => 0,
            x => x,
        }
    };
    // (degrees, minutes, seconds, 16ths of a second)
    let dms = |a: i64| (a / 57600, a % 57600 / 960, a % 960 / 16, a % 16);
    let (la, lo) = (dms(lat16), dms(lon16));
    let dd = la.0 + lo.0 * 181;
    Some(vec![
        (dd >> 8) as u8,
        dd as u8,
        (((la.1 / 4) << 4) | (lo.1 / 4)) as u8,
        (((la.1 % 4) << 6) | ((lo.1 % 4) << 4) | ((la.2 / 15) << 2) | (lo.2 / 15)) as u8,
        (((la.2 % 15) << 4) | (lo.2 % 15)) as u8,
        ((la.3 << 4) | lo.3) as u8,
    ])
}

/// libzim's `countWords`: runs of non-whitespace bytes (C `isspace`).
pub fn count_words(text: &str) -> u32 {
    text.split([' ', '\t', '\n', '\x0b', '\x0c', '\r'])
        .filter(|w| !w.is_empty())
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdata::{Archive, archives, index};
    use crate::text::stemmer;
    use std::collections::BTreeMap;

    /// The climate archive's title index, inverted: (path, terms) by docid − 1.
    fn real_title_docs(a: &Archive) -> Vec<(String, Terms)> {
        let db = index(a, false);
        let mut docs: BTreeMap<u32, Terms> = BTreeMap::new();
        for (t, _) in db.terms(b"").unwrap() {
            let mut pl = db.postlist(&t).unwrap().unwrap();
            while pl.next().unwrap() {
                let pos = db.positions(pl.did(), &t).unwrap();
                docs.entry(pl.did())
                    .or_default()
                    .insert(t.clone(), (pl.wdf(), pos));
            }
        }
        (1..=db.stats.doccount)
            .map(|d| {
                let path = String::from_utf8(db.data(d).unwrap().unwrap()).unwrap();
                (path, docs.remove(&d).unwrap_or_default())
            })
            .collect()
    }

    #[test]
    fn titles_match_libzim() {
        for a in archives() {
            titles_match(&a);
        }
    }

    fn titles_match(a: &Archive) {
        let st = stemmer("eng");
        let db = index(a, false);
        let real = real_title_docs(a);
        let mut bad = vec![];
        for (did, (path, want)) in real.iter().enumerate() {
            let title = db.value(did as u32 + 1, 0).unwrap().unwrap();
            let got = title_terms(std::str::from_utf8(&title).unwrap(), st.as_ref());
            if &got != want {
                if bad.is_empty() {
                    let show = |t: &Terms| {
                        t.iter()
                            .map(|(k, v)| format!("{}:{:?}", String::from_utf8_lossy(k), v))
                            .collect::<Vec<_>>()
                    };
                    eprintln!("{path}\n got {:?}\nwant {:?}", show(&got), show(want));
                }
                bad.push(path.clone());
            }
        }
        assert!(
            bad.is_empty(),
            "{} of {} differ: {:?}",
            bad.len(),
            real.len(),
            &bad[..bad.len().min(10)]
        );
    }

    /// Every full-text document regenerates libzim's terms and wdfs, from
    /// the article HTML and title.
    #[test]
    fn fulltext_matches_libzim() {
        for a in archives() {
            let db = index(&a, true);
            let mut real: BTreeMap<u32, BTreeMap<Vec<u8>, u32>> = BTreeMap::new();
            for (t, _) in db.terms(b"").unwrap() {
                let mut pl = db.postlist(&t).unwrap().unwrap();
                while pl.next().unwrap() {
                    real.entry(pl.did())
                        .or_default()
                        .insert(t.clone(), pl.wdf());
                }
            }
            let st = stemmer("eng");
            let mut bad = vec![];
            for did in 1..=db.stats.doccount {
                let path = String::from_utf8(db.data(did).unwrap().unwrap()).unwrap();
                let e = a.entry_by_path(&path[2..]).unwrap();
                let html = a.content(&e).unwrap();
                let h = crate::html::parse(&String::from_utf8_lossy(&html));
                let got = fulltext_terms(
                    &remove_accents(&h.dump),
                    &remove_accents(&e.title),
                    &remove_accents(&h.keywords),
                    st.as_ref(),
                );
                let got: BTreeMap<Vec<u8>, u32> = got.into_iter().map(|(k, v)| (k, v.0)).collect();
                let want = real.remove(&did).unwrap_or_default();
                if got != want {
                    if bad.is_empty() {
                        let only = |a: &BTreeMap<Vec<u8>, u32>, b: &BTreeMap<Vec<u8>, u32>| {
                            a.iter()
                                .filter(|(k, v)| b.get(*k) != Some(v))
                                .map(|(k, v)| format!("{}:{v}", String::from_utf8_lossy(k)))
                                .take(15)
                                .collect::<Vec<_>>()
                        };
                        eprintln!(
                            "{path}\n ours {:?}\nlibzim {:?}",
                            only(&got, &want),
                            only(&want, &got)
                        );
                    }
                    bad.push(path);
                }
            }
            assert!(
                bad.is_empty(),
                "{} of {} differ",
                bad.len(),
                db.stats.doccount
            );
        }
    }

    #[test]
    fn words() {
        assert_eq!(count_words("  a b\tc\n\n d "), 4);
        assert_eq!(count_words(""), 0);
    }
}
