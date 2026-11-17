//! HTML to text, as libzim indexes it: a port of Xapian omega's `HtmlParser`
//! and `MyHtmlParser` as patched by libzim (`src/xapian/htmlparse.{cc,h}`,
//! `myhtmlparse.{cc,h}`, `namedentities.h`), called the way
//! `writer/defaultIndexData.h` calls it: `parse_html(text, "UTF-8", true)`,
//! exceptions swallowed. Works on bytes like the C++.

pub(crate) struct Html {
    pub dump: String,
    #[allow(dead_code)] // parsed as libzim does; libzim doesn't use it either
    pub title: String,
    pub keywords: String,
    pub indexing_allowed: bool,
    /// (latitude, longitude), parsed as C++ `float`s.
    pub geo: Option<(f64, f64)>,
}

pub(crate) fn parse(html: &str) -> Html {
    let mut p = Parser {
        params: Vec::new(),
        in_script: false,
        in_script_tag: false,
        in_style_tag: false,
        pending_space: false,
        indexing_allowed: true,
        title: Vec::new(),
        keywords: Vec::new(),
        dump: Vec::new(),
        geo: None,
    };
    p.parse_html(html.as_bytes());
    let s = |v: Vec<u8>| {
        String::from_utf8(v).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    };
    Html {
        dump: s(p.dump),
        title: s(p.title),
        keywords: s(p.keywords),
        indexing_allowed: p.indexing_allowed,
        geo: p.geo,
    }
}

struct Parser {
    /// Attributes of the current opening tag; the first of a name wins.
    params: Vec<(Vec<u8>, Vec<u8>)>,
    /// HtmlParser's flag: inside <script>, `<` + letter isn't a tag.
    in_script: bool,
    in_script_tag: bool,
    in_style_tag: bool,
    pending_space: bool,
    indexing_allowed: bool,
    title: Vec<u8>,
    keywords: Vec<u8>,
    dump: Vec<u8>,
    geo: Option<(f64, f64)>,
}

/// C `isspace`: space, \t \n \v \f \r.
fn is_space(c: u8) -> bool {
    c == b' ' || (b'\t'..=b'\r').contains(&c)
}

/// MyHtmlParser's WHITESPACE: " \t\n\r".
fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

/// Index of the first byte at or after `from` matching `f`, else `s.len()`.
fn find(s: &[u8], from: usize, f: impl Fn(u8) -> bool) -> usize {
    let from = from.min(s.len());
    s[from..]
        .iter()
        .position(|&c| f(c))
        .map_or(s.len(), |i| from + i)
}

/// `s[i]`, or the NUL past the end of a `std::string`.
fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

fn lower(s: &[u8]) -> Vec<u8> {
    s.to_ascii_lowercase()
}

impl Parser {
    fn param(&self, name: &[u8]) -> Option<Vec<u8>> {
        self.params
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    }

    /// HtmlParser::parse_html. Returning early is the C++ `break` or a
    /// swallowed `throw`.
    fn parse_html(&mut self, b: &[u8]) {
        let n = b.len();
        let mut start = 0;
        loop {
            // Skip to a tag, comment, or the end; ignore isolated `<`.
            let mut p = start;
            loop {
                p = find(b, p, |c| c == b'<');
                if p == n {
                    break;
                }
                let ch = at(b, p + 1);
                // `<?` always breaks: every path of the XML-declaration
                // sniffing does, and the charset it finds is unused.
                if (!self.in_script && ch.is_ascii_alphabetic()) || matches!(ch, b'/' | b'!' | b'?')
                {
                    break;
                }
                p += 1;
            }
            if p > start {
                let text = decode_entities(&b[start..p]);
                self.process_text(&text);
            }
            if p == n {
                return;
            }
            start = p + 1;
            if b[start] == b'!' {
                start += 1;
                if start == n {
                    return;
                }
                start += 1;
                if start == n {
                    return;
                }
                if b[start - 1] == b'-' && b[start] == b'-' {
                    // Comment.
                    start += 1;
                    let close = find(b, start, |c| c == b'>');
                    // An unterminated comment swallows the rest.
                    if close == n {
                        return;
                    }
                    p = close;
                    while p != n && (b[p - 1] != b'-' || b[p - 2] != b'-') {
                        p = find(b, p + 1, |c| c == b'>');
                    }
                    if p != n {
                        if p - start == 15 && &b[start..p - 2] == b"htdig_noindex" {
                            let end = b"<!--/htdig_noindex-->";
                            match b[p + 1..].windows(end.len()).position(|w| w == end) {
                                None => return,
                                Some(i) => start = p + 1 + i + end.len(),
                            }
                            continue;
                        }
                        start = p;
                    } else {
                        // No `-->`: skip to the first `>`, as Netscape does.
                        start = close;
                    }
                } else {
                    // SGML declaration.
                    start = find(b, start - 1, |c| c == b'>');
                    if start == n {
                        return;
                    }
                }
                start += 1;
            } else if b[start] == b'?' {
                // PHP: swallow until `?>` or the end.
                start += 1;
                if start == n {
                    return;
                }
                start = find(b, start + 1, |c| c == b'>');
                while start != n && b[start - 1] != b'?' {
                    start = find(b, start + 1, |c| c == b'>');
                }
                if start != n {
                    start += 1;
                }
            } else {
                let closing = b[start] == b'/';
                if closing {
                    start = find(b, start + 1, |c| !is_space(c));
                }
                p = start;
                start = find(b, start, |c| {
                    !(c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b':'))
                });
                let tag = lower(&b[p..start]);
                if closing {
                    if self.closing_tag(&tag) {
                        return;
                    }
                    if self.in_script && tag == b"script" {
                        self.in_script = false;
                    }
                    // Ignore any bogus parameters on closing tags.
                    p = find(b, start, |c| c == b'>');
                    if p == n {
                        return;
                    }
                    start = p + 1;
                } else {
                    while start < n && b[start] != b'>' {
                        p = find(b, start, |c| is_space(c) || c == b'=' || c == b'>');
                        let name = &b[start..p];
                        p = find(b, p, |c| !is_space(c));
                        start = p;
                        if start != n && b[start] == b'=' {
                            start = find(b, start + 1, |c| !is_space(c));
                            p = n;
                            let quote = at(b, start);
                            if quote == b'"' || quote == b'\'' {
                                start += 1;
                                p = find(b, start, |c| c == quote);
                            }
                            if p == n {
                                // Unquoted, or no closing quote.
                                p = find(b, start, |c| is_space(c) || c == b'>');
                            }
                            let value = b[start..p].to_vec();
                            start = find(b, p, |c| !is_space(c));
                            if !name.is_empty() {
                                let name = lower(name);
                                if !self.params.iter().any(|(k, _)| *k == name) {
                                    self.params.push((name, value));
                                }
                            }
                        }
                    }
                    if self.opening_tag(&tag) {
                        return;
                    }
                    self.params.clear();
                    // In <script> ignore opening tags, to survive "a<b".
                    if tag == b"script" {
                        self.in_script = true;
                    }
                    if start != n && b[start] == b'>' {
                        start += 1;
                    }
                }
            }
        }
    }

    /// MyHtmlParser::process_text: collapse runs of " \t\n\r" to one space.
    fn process_text(&mut self, text: &[u8]) {
        if text.is_empty() || self.in_script_tag || self.in_style_tag {
            return;
        }
        let n = text.len();
        let mut b = find(text, 0, |c| !is_ws(c));
        if b != 0 {
            self.pending_space = true;
        }
        while b != n {
            if self.pending_space && !self.dump.is_empty() {
                self.dump.push(b' ');
            }
            let e = find(text, b, is_ws);
            self.pending_space = e != n;
            self.dump.extend_from_slice(&text[b..e]);
            if e == n {
                return;
            }
            b = find(text, e + 1, |c| !is_ws(c));
        }
    }

    /// MyHtmlParser::opening_tag. Returns true where the C++ throws.
    fn opening_tag(&mut self, tag: &[u8]) -> bool {
        match tag {
            b"body" => self.dump.clear(),
            b"style" => self.in_style_tag = true,
            b"script" => self.in_script_tag = true,
            b"meta" => return self.meta(),
            b"address" | b"blockquote" | b"br" | b"center" | b"dd" | b"dir" | b"div" | b"dl"
            | b"dt" | b"embed" | b"fieldset" | b"form" | b"hr" | b"h1" | b"h2" | b"h3" | b"h4"
            | b"h5" | b"h6" | b"iframe" | b"img" | b"isindex" | b"input" | b"keygen"
            | b"legend" | b"li" | b"listing" | b"marquee" | b"menu" | b"multicol" | b"ol"
            | b"option" | b"p" | b"pre" | b"plaintext" | b"q" | b"select" | b"table" | b"td"
            | b"textarea" | b"th" | b"ul" | b"xmp" => self.pending_space = true,
            _ => {}
        }
        false
    }

    /// MyHtmlParser::closing_tag. Returns true where the C++ throws.
    fn closing_tag(&mut self, tag: &[u8]) -> bool {
        match tag {
            b"body" => return true,
            b"style" => self.in_style_tag = false,
            b"script" => self.in_script_tag = false,
            b"title" => {
                if self.title.is_empty() {
                    std::mem::swap(&mut self.title, &mut self.dump);
                }
            }
            b"address" | b"blockquote" | b"br" | b"center" | b"dd" | b"dir" | b"div" | b"dl"
            | b"dt" | b"fieldset" | b"form" | b"hr" | b"h1" | b"h2" | b"h3" | b"h4" | b"h5"
            | b"h6" | b"iframe" | b"legend" | b"li" | b"listing" | b"marquee" | b"menu" | b"ol"
            | b"option" | b"p" | b"pre" | b"q" | b"select" | b"table" | b"td" | b"textarea"
            | b"th" | b"ul" | b"xmp" => self.pending_space = true,
            _ => {}
        }
        false
    }

    /// `<meta name=... content=...>`. The description (C++ `sample`) is
    /// unused by libzim, and charset metas are ignored (charset_from_meta).
    fn meta(&mut self) -> bool {
        let (Some(content), Some(name)) = (self.param(b"content"), self.param(b"name")) else {
            return false;
        };
        match &lower(&name)[..] {
            b"keywords" => {
                if !self.keywords.is_empty() {
                    self.keywords.push(b' ');
                }
                self.keywords.extend(decode_entities(&content));
            }
            b"robots" => {
                let c = lower(&decode_entities(&content));
                let has = |w: &[u8]| c.windows(w.len()).any(|x| x == w);
                if has(b"none") || has(b"noindex") {
                    self.indexing_allowed = false;
                    return true;
                }
            }
            b"geo.position" => {
                if let Some(sep) = content.iter().position(|&c| c == b';') {
                    self.geo = Some((
                        stof(&content[..sep]) as f64,
                        stof(&content[sep + 1..]) as f64,
                    ));
                }
            }
            _ => {}
        }
        false
    }
}

/// `std::istringstream(s) >> float` (libstdc++): skip whitespace, take the
/// longest number-shaped prefix, 0 if that doesn't convert, ±max on overflow.
fn stof(s: &[u8]) -> f32 {
    let mut i = find(s, 0, |c| !is_space(c));
    let st = i;
    let digits = |i: &mut usize| {
        let b = *i;
        while *i < s.len() && s[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > b
    };
    if matches!(at(s, i), b'+' | b'-') {
        i += 1;
    }
    let mut mantissa = digits(&mut i);
    if at(s, i) == b'.' {
        i += 1;
        mantissa |= digits(&mut i);
    }
    if mantissa && matches!(at(s, i), b'e' | b'E') {
        i += 1;
        if matches!(at(s, i), b'+' | b'-') {
            i += 1;
        }
        digits(&mut i);
    }
    let v: f32 = std::str::from_utf8(&s[st..i])
        .unwrap()
        .parse()
        .unwrap_or(0.0);
    if v.is_infinite() {
        f32::MAX.copysign(v)
    } else {
        v
    }
}

/// HtmlParser::decode_entities: named (case-sensitive, `;` optional),
/// decimal and hex entities; unknown ones are left as they are.
fn decode_entities(s: &[u8]) -> Vec<u8> {
    let n = s.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    loop {
        let amp = find(s, i, |c| c == b'&');
        out.extend_from_slice(&s[i..amp]);
        if amp == n {
            return out;
        }
        let mut p = amp + 1;
        let mut end;
        let mut val: u32 = 0;
        if at(s, p) == b'#' {
            p += 1;
            if matches!(at(s, p), b'x' | b'X') {
                p += 1;
                end = find(s, p, |c| !c.is_ascii_hexdigit());
                // sscanf("%x"): strtoul saturating at ULONG_MAX, truncated.
                let d = std::str::from_utf8(&s[p..end]).unwrap();
                if !d.is_empty() {
                    val = u64::from_str_radix(d, 16).unwrap_or(u64::MAX) as u32;
                }
            } else {
                end = find(s, p, |c| !c.is_ascii_digit());
                // atoi: (int)strtol, saturating at LONG_MAX, truncated.
                let d = std::str::from_utf8(&s[p..end]).unwrap();
                if !d.is_empty() {
                    val = d
                        .parse::<u64>()
                        .map_or(i64::MAX as u64, |v| v.min(i64::MAX as u64))
                        as u32;
                }
            }
        } else {
            end = find(s, p, |c| !c.is_ascii_alphanumeric());
            if let Some(&(_, v)) = NAMED_ENTS.iter().find(|(k, _)| k.as_bytes() == &s[p..end]) {
                val = v;
            }
        }
        if end < n && s[end] == b';' {
            end += 1;
        }
        i = end;
        if val == 0 {
            out.extend_from_slice(&s[amp..end]);
        } else if !utf8(val, &mut out) && end < n {
            // Nothing was inserted, and the C++ resumes its search one byte
            // past the entity's position, so the next byte is taken as is.
            out.push(s[end]);
            i = end + 1;
        }
    }
}

/// Append `val` as `char(val)` or Xapian's `nonascii_to_utf8`, which encodes
/// anything below 0x200000 (surrogates too) and drops the rest. Returns
/// whether it appended anything.
fn utf8(val: u32, out: &mut Vec<u8>) -> bool {
    let c = |shift: u32| 0x80 | ((val >> shift) & 0x3f) as u8;
    match val {
        0..0x80 => out.push(val as u8),
        0x80..0x800 => out.extend([0xc0 | (val >> 6) as u8, c(0)]),
        0x800..0x10000 => out.extend([0xe0 | (val >> 12) as u8, c(6), c(0)]),
        0x10000..0x200000 => out.extend([0xf0 | (val >> 18) as u8, c(12), c(6), c(0)]),
        _ => return false,
    }
    true
}

/// namedentities.h: HTML 4's character entities, plus `apos`.
#[rustfmt::skip]
static NAMED_ENTS: &[(&str, u32)] = &[
    ("quot", 34), ("amp", 38), ("apos", 39), ("lt", 60),
    ("gt", 62), ("nbsp", 160), ("iexcl", 161), ("cent", 162),
    ("pound", 163), ("curren", 164), ("yen", 165), ("brvbar", 166),
    ("sect", 167), ("uml", 168), ("copy", 169), ("ordf", 170),
    ("laquo", 171), ("not", 172), ("shy", 173), ("reg", 174),
    ("macr", 175), ("deg", 176), ("plusmn", 177), ("sup2", 178),
    ("sup3", 179), ("acute", 180), ("micro", 181), ("para", 182),
    ("middot", 183), ("cedil", 184), ("sup1", 185), ("ordm", 186),
    ("raquo", 187), ("frac14", 188), ("frac12", 189), ("frac34", 190),
    ("iquest", 191), ("Agrave", 192), ("Aacute", 193), ("Acirc", 194),
    ("Atilde", 195), ("Auml", 196), ("Aring", 197), ("AElig", 198),
    ("Ccedil", 199), ("Egrave", 200), ("Eacute", 201), ("Ecirc", 202),
    ("Euml", 203), ("Igrave", 204), ("Iacute", 205), ("Icirc", 206),
    ("Iuml", 207), ("ETH", 208), ("Ntilde", 209), ("Ograve", 210),
    ("Oacute", 211), ("Ocirc", 212), ("Otilde", 213), ("Ouml", 214),
    ("times", 215), ("Oslash", 216), ("Ugrave", 217), ("Uacute", 218),
    ("Ucirc", 219), ("Uuml", 220), ("Yacute", 221), ("THORN", 222),
    ("szlig", 223), ("agrave", 224), ("aacute", 225), ("acirc", 226),
    ("atilde", 227), ("auml", 228), ("aring", 229), ("aelig", 230),
    ("ccedil", 231), ("egrave", 232), ("eacute", 233), ("ecirc", 234),
    ("euml", 235), ("igrave", 236), ("iacute", 237), ("icirc", 238),
    ("iuml", 239), ("eth", 240), ("ntilde", 241), ("ograve", 242),
    ("oacute", 243), ("ocirc", 244), ("otilde", 245), ("ouml", 246),
    ("divide", 247), ("oslash", 248), ("ugrave", 249), ("uacute", 250),
    ("ucirc", 251), ("uuml", 252), ("yacute", 253), ("thorn", 254),
    ("yuml", 255), ("OElig", 338), ("oelig", 339), ("Scaron", 352),
    ("scaron", 353), ("Yuml", 376), ("fnof", 402), ("circ", 710),
    ("tilde", 732), ("Alpha", 913), ("Beta", 914), ("Gamma", 915),
    ("Delta", 916), ("Epsilon", 917), ("Zeta", 918), ("Eta", 919),
    ("Theta", 920), ("Iota", 921), ("Kappa", 922), ("Lambda", 923),
    ("Mu", 924), ("Nu", 925), ("Xi", 926), ("Omicron", 927),
    ("Pi", 928), ("Rho", 929), ("Sigma", 931), ("Tau", 932),
    ("Upsilon", 933), ("Phi", 934), ("Chi", 935), ("Psi", 936),
    ("Omega", 937), ("alpha", 945), ("beta", 946), ("gamma", 947),
    ("delta", 948), ("epsilon", 949), ("zeta", 950), ("eta", 951),
    ("theta", 952), ("iota", 953), ("kappa", 954), ("lambda", 955),
    ("mu", 956), ("nu", 957), ("xi", 958), ("omicron", 959),
    ("pi", 960), ("rho", 961), ("sigmaf", 962), ("sigma", 963),
    ("tau", 964), ("upsilon", 965), ("phi", 966), ("chi", 967),
    ("psi", 968), ("omega", 969), ("thetasym", 977), ("upsih", 978),
    ("piv", 982), ("ensp", 8194), ("emsp", 8195), ("thinsp", 8201),
    ("zwnj", 8204), ("zwj", 8205), ("lrm", 8206), ("rlm", 8207),
    ("ndash", 8211), ("mdash", 8212), ("lsquo", 8216), ("rsquo", 8217),
    ("sbquo", 8218), ("ldquo", 8220), ("rdquo", 8221), ("bdquo", 8222),
    ("dagger", 8224), ("Dagger", 8225), ("bull", 8226), ("hellip", 8230),
    ("permil", 8240), ("prime", 8242), ("Prime", 8243), ("lsaquo", 8249),
    ("rsaquo", 8250), ("oline", 8254), ("frasl", 8260), ("euro", 8364),
    ("image", 8465), ("weierp", 8472), ("real", 8476), ("trade", 8482),
    ("alefsym", 8501), ("larr", 8592), ("uarr", 8593), ("rarr", 8594),
    ("darr", 8595), ("harr", 8596), ("crarr", 8629), ("lArr", 8656),
    ("uArr", 8657), ("rArr", 8658), ("dArr", 8659), ("hArr", 8660),
    ("forall", 8704), ("part", 8706), ("exist", 8707), ("empty", 8709),
    ("nabla", 8711), ("isin", 8712), ("notin", 8713), ("ni", 8715),
    ("prod", 8719), ("sum", 8721), ("minus", 8722), ("lowast", 8727),
    ("radic", 8730), ("prop", 8733), ("infin", 8734), ("ang", 8736),
    ("and", 8743), ("or", 8744), ("cap", 8745), ("cup", 8746),
    ("int", 8747), ("there4", 8756), ("sim", 8764), ("cong", 8773),
    ("asymp", 8776), ("ne", 8800), ("equiv", 8801), ("le", 8804),
    ("ge", 8805), ("sub", 8834), ("sup", 8835), ("nsub", 8836),
    ("sube", 8838), ("supe", 8839), ("oplus", 8853), ("otimes", 8855),
    ("perp", 8869), ("sdot", 8901), ("lceil", 8968), ("rceil", 8969),
    ("lfloor", 8970), ("rfloor", 8971), ("lang", 9001), ("rang", 9002),
    ("loz", 9674), ("spades", 9824), ("clubs", 9827), ("hearts", 9829),
    ("diams", 9830),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn dump(html: &str) -> String {
        parse(html).dump
    }

    #[test]
    fn block_tags_space_inline_tags_dont() {
        assert_eq!(dump("a<b>b</b>c<i>d</i>e"), "abcde");
        assert_eq!(dump("a<p>b</p>c<br>d<div>e</div>f<td>g"), "a b c d e f g");
        assert_eq!(dump("a<h2>b</h2>c<hr>d<h7>e"), "a b c de");
        assert_eq!(dump("a<img src=x>b<embed>c</embed>d"), "a b cd");
        assert_eq!(dump("<p>  a \n\t b  </p>"), "a b");
        // A pending space is only emitted between words, never leading.
        assert_eq!(dump("<p><p>x"), "x");
        // \f and \v aren't WHITESPACE: kept verbatim.
        assert_eq!(dump("a\x0cb"), "a\x0cb");
    }

    #[test]
    fn body_resets_and_ends() {
        assert_eq!(dump("junk<body>kept</body>dropped"), "kept");
        let h = parse("<title>T &amp; U</title>x<p>y");
        assert_eq!((h.title.as_str(), h.dump.as_str()), ("T & U", "x y"));
    }

    #[test]
    fn script_style_and_comments() {
        assert_eq!(dump("a<script>if (a<b) x</script>b"), "ab");
        assert_eq!(dump("a<style>p{}</style> b"), "a b");
        // </div> inside a script is still a tag, and still sets a space.
        assert_eq!(dump("a<script>x</div>y</script>b"), "a b");
        assert_eq!(dump("a<!-- c -->b<!DOCTYPE html>c<?php x ?>d"), "abcd");
        assert_eq!(dump("a<!--htdig_noindex-->x<!--/htdig_noindex-->b"), "ab");
        assert_eq!(dump("a<!--htdig_noindex-->x"), "a");
        assert_eq!(dump("a<!-- unterminated"), "a");
        assert_eq!(dump("a < b <3 c<"), "a < b <3 c<");
    }

    #[test]
    fn meta() {
        let h = parse(
            r#"<meta name="keywords" content="a &amp; b"><META NAME=Keywords content='c'>
            <meta name="geo.position" content="51.5;-0.12"><meta name=robots content="NoIndex">x"#,
        );
        assert_eq!(h.keywords, "a & b c");
        assert_eq!(h.geo, Some((51.5f32 as f64, -0.12f32 as f64)));
        assert!(!h.indexing_allowed);
        assert_eq!(h.dump, ""); // parsing stops at the robots meta
        assert!(!parse(r#"<meta content="none" name="robots">"#).indexing_allowed);
        assert!(parse(r#"<meta name="robots" content="index">"#).indexing_allowed);
        assert_eq!(
            parse(r#"<meta name="geo.position" content="x;2">"#).geo,
            Some((0.0, 2.0))
        );
    }

    #[test]
    fn entities() {
        assert_eq!(
            dump("&lt;&gt&amp;amp; &eacute;&nbsp;&Omega;"),
            "<>&amp; é\u{a0}Ω"
        );
        assert_eq!(dump("&#65;&#x42;&#X43 &#x1F600;"), "ABC \u{1F600}");
        assert_eq!(
            dump("&bogus; &AMP; &#; &#x; & x"),
            "&bogus; &AMP; &#; &#x; & x"
        );
        // Decoded whitespace collapses like literal whitespace.
        assert_eq!(dump("a&#10;&#32;b"), "a b");
        // Out of range: removed, and the byte after it skipped by the search.
        assert_eq!(dump("a&#x300000;&amp;"), "a&amp;");
    }

    /// libzim's `countWords`: runs of non-`isspace` bytes.
    fn count_words(s: &str) -> u32 {
        s.as_bytes()
            .split(|&c| is_space(c))
            .filter(|w| !w.is_empty())
            .count() as u32
    }

    /// Every article's word count against the one libzim stored in value
    /// slot 1 of the climate archive's fulltext index.
    #[test]
    fn word_counts_match_libzim() {
        let a = crate::testdata::climate();
        let db = crate::testdata::index(&a, true);
        let mut bad = Vec::new();
        for did in 1..=db.stats.doccount {
            let path = db.data(did).unwrap().unwrap();
            let wc: u32 = std::str::from_utf8(&db.value(did, 1).unwrap().unwrap())
                .unwrap()
                .parse()
                .unwrap();
            let e = a
                .entry_by_path(std::str::from_utf8(&path[2..]).unwrap())
                .unwrap();
            let html = a.content(&e).unwrap();
            let got = count_words(&parse(&String::from_utf8_lossy(&html)).dump);
            if got != wc {
                bad.push((String::from_utf8_lossy(&path).into_owned(), wc, got));
            }
        }
        assert!(
            bad.is_empty(),
            "{} of {} differ: {:?}",
            bad.len(),
            db.stats.doccount,
            &bad[..bad.len().min(20)]
        );
    }
}
