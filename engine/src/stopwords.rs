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

pub fn parse(text: &str) -> HashSet<&str> {
    text.split('\n').filter(|word| !word.is_empty()).collect()
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
    fn writer_and_reader_use_the_index_stoplist() {
        for (language, enabled) in [("en", true), ("eng", false)] {
            let mut writer = IndexWriter::fulltext(language);
            writer.add_text("Corals", "Corals", "the corals are diverse");
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
