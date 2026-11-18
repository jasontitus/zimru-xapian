// SPDX-License-Identifier: GPL-3.0-or-later
//! Unicode properties and canonical accent removal for search, derived from
//! Unicode 18.0.0. Regenerate the checked-in data with examples/generate_unicode.rs.

const DIRECTORY: &[u8; 0x1100] = include_bytes!("unicode/flags.index");
const PAGES: &[u8] = include_bytes!("unicode/flags.pages");
fn decomposition(c: char) -> &'static str {
    include!("unicode/decompositions.rs")
}

const WORD: u8 = 1;
const DECIMAL: u8 = 2;
const STEM: u8 = 4;
const MARK: u8 = 8;
const DECOMPOSE: u8 = 16;
const COMPOSE_SECOND: u8 = 32;

fn flags(c: char) -> u8 {
    let scalar = c as usize;
    let page = usize::from(DIRECTORY[scalar >> 8]);
    PAGES[page * 256 + (scalar & 255)]
}

pub(crate) fn is_wordchar(c: char) -> bool {
    flags(c) & WORD != 0
}

pub(crate) fn is_digit(c: char) -> bool {
    flags(c) & DECIMAL != 0
}

pub(crate) fn should_stem(c: char) -> bool {
    flags(c) & STEM != 0
}

/// Canonical Hangul composition from Unicode Standard Annex #15. The other
/// pairs that can survive mark removal are emitted by the generator.
fn compose(first: char, second: char) -> Option<char> {
    let a = first as u32;
    let b = second as u32;
    if matches!(a, 0x1100..=0x1112) && matches!(b, 0x1161..=0x1175) {
        return char::from_u32(0xac00 + ((a - 0x1100) * 21 + (b - 0x1161)) * 28);
    }
    if matches!(a, 0xac00..=0xd7a3)
        && (a - 0xac00).is_multiple_of(28)
        && matches!(b, 0x11a8..=0x11c2)
    {
        return char::from_u32(a + (b - 0x11a7));
    }
    include!("unicode/compositions.rs")
}

fn push_starter(out: &mut String, c: char, bits: u8) {
    if (bits & COMPOSE_SECOND != 0
        || matches!(c, '\u{1161}'..='\u{1175}' | '\u{11a8}'..='\u{11c2}'))
        && let Some(composed) = out.chars().next_back().and_then(|first| compose(first, c))
    {
        out.pop();
        out.push(composed);
    } else {
        out.push(c);
    }
}

/// Apply libzim's Lower; NFD; remove marks; NFC transform. Every surviving
/// decomposed scalar has combining class zero. Removing marks therefore
/// preserves their input order and NFC only needs the previous starter.
pub(crate) fn remove_accents(input: &str) -> String {
    if input.is_ascii() {
        return input.to_ascii_lowercase();
    }
    // Whole-string lowercase preserves final sigma context and the Rust
    // toolchain's lowercase version, as in the original engine.
    let lower = input.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    for c in lower.chars() {
        let bits = flags(c);
        if bits & MARK != 0 {
            continue;
        }
        if bits & DECOMPOSE != 0 {
            for starter in decomposition(c).chars() {
                push_starter(&mut out, starter, flags(starter));
            }
        } else {
            push_starter(&mut out, c, bits);
        }
    }
    out
}
