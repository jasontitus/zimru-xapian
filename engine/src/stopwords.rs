//! Exact language-code lookup, following libzim and xapianbuilder.
//! Sources and MIT notices for the Stopwords ISO lists are in LICENSES/.
use std::collections::HashSet;

pub fn bundled(code: &str) -> &'static str {
    macro_rules! select {
        ($($code:literal),* $(,)?) => {
            match code {
                $($code => include_str!(concat!("../data/stopwords/", $code)),)*
                _ => "",
            }
        };
    }
    select!(
        "af", "ar", "bg", "bn", "br", "ca", "cs", "da", "de", "el", "en", "eo", "es", "et", "eu",
        "fa", "fi", "fr", "ga", "gl", "gu", "ha", "he", "hi", "hr", "hu", "hy", "id", "it", "ja",
        "ko", "ku", "la", "lt", "lv", "mr", "ms", "nl", "no", "pl", "pt", "ro", "ru", "sk", "sl",
        "so", "st", "sv", "sw", "th", "tl", "tr", "uk", "ur", "vi", "yo", "zh", "zu"
    )
}

pub fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n').filter(|word| !word.is_empty())
}

/// Borrow the words from the decoded metadata and grow the index only as
/// distinct words are encountered. Empty lines and repeats cost no entries.
pub fn from_metadata(bytes: &[u8], limit: usize) -> crate::Result<HashSet<&str>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| crate::Error::Format("invalid UTF-8 in index stopwords".into()))?;
    let mut distinct = HashSet::new();
    for word in words(text) {
        if distinct.len() == limit {
            if !distinct.contains(word) {
                return Err(crate::Error::Invalid(format!(
                    "index exceeds the {limit} distinct stopword limit"
                )));
            }
        } else {
            distinct.insert(word);
        }
    }
    Ok(distinct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Index, IndexWriter};
    use std::io::Cursor;

    fn open(writer: IndexWriter) -> Index {
        let mut bytes = Cursor::new(Vec::new());
        writer.write(&mut bytes, [0; 16]).unwrap();
        Index::open(Box::new(bytes.into_inner())).unwrap()
    }

    #[test]
    fn stoplist_limit_counts_distinct_nonempty_words() {
        assert_eq!(
            from_metadata(b"\nbeta\nalpha\nalpha\n\nbeta\n", 2).unwrap(),
            HashSet::from(["alpha", "beta"])
        );
        assert!(matches!(
            from_metadata(b"alpha\nalpha\nbeta", 1),
            Err(crate::Error::Invalid(_))
        ));
        assert!(from_metadata(b"\n\n", 0).unwrap().is_empty());
        assert!(matches!(
            from_metadata(&[0xff], 8),
            Err(crate::Error::Format(_))
        ));
    }

    #[test]
    fn writer_and_reader_use_the_index_stoplist() {
        for (language, enabled) in [("en", true), ("eng", false)] {
            let mut writer = IndexWriter::fulltext(language);
            writer
                .add_text("Corals", "Corals", "the corals are diverse")
                .unwrap();
            let index = open(writer);
            assert_eq!(index.db.freqs(b"the").unwrap().0, u32::from(!enabled));
            assert_eq!(
                index.db.metadata("stopwords").unwrap().unwrap_or_default(),
                bundled(language).as_bytes()
            );
            let hits = index.search("the corals", 10, "de").unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].path, "C/Corals");
            assert_eq!(
                index.search("the", 10, "de").unwrap().len(),
                usize::from(!enabled)
            );
        }
    }

    #[test]
    fn title_suggestions_keep_stopwords() {
        let mut writer = IndexWriter::title("en");
        writer.add_title("Treaty", "The Treaty", None).unwrap();
        let index = open(writer);
        assert_eq!(
            index.suggest("the tre", 10, "eng").unwrap(),
            vec!["C/Treaty"]
        );
        assert_eq!(index.db.freqs(b"the").unwrap().0, 1);
    }
}
