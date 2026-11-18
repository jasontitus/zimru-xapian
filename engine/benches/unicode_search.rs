// SPDX-License-Identifier: GPL-3.0-or-later
//! Compare the previous ICU pipeline to the release path on identical string and property inputs.
#![forbid(unsafe_code)]
#[allow(dead_code)]
#[path = "../src/unicode.rs"]
mod unicode;

use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use icu_properties::{
    CodePointMapData,
    props::{GeneralCategory as GC, GeneralCategoryGroup as G},
};
use std::{hint::black_box, time::Instant};

fn icu_accents(input: &str) -> String {
    if input.is_ascii() {
        return input.to_ascii_lowercase();
    }
    let lower = input.to_lowercase();
    let categories = CodePointMapData::<GC>::new();
    let decomposed = DecomposingNormalizerBorrowed::new_nfd().normalize_iter(lower.chars());
    let filtered = decomposed.filter(|&c| !G::Mark.contains(categories.get(c)));
    ComposingNormalizerBorrowed::new_nfc()
        .normalize_iter(filtered)
        .collect()
}

fn icu_word(c: char) -> bool {
    let group = G::Letter
        .union(G::Mark)
        .union(G::Number)
        .union(G::ConnectorPunctuation);
    group.contains(CodePointMapData::<GC>::new().get(c))
}

fn measure<T>(f: impl Fn() -> T) -> u128 {
    let start = Instant::now();
    for _ in 0..256 {
        black_box(f());
    }
    start.elapsed().as_nanos()
}

fn compare<T: PartialEq>(
    name: &str,
    baseline: impl Fn() -> T,
    candidate: impl Fn() -> T,
    scalars: usize,
) {
    assert!(baseline() == candidate());
    let mut samples = [Vec::new(), Vec::new()];
    for n in 0..22 {
        let (a, b) = if n % 2 == 0 {
            (measure(&baseline), measure(&candidate))
        } else {
            let b = measure(&candidate);
            (measure(&baseline), b)
        };
        if n >= 2 {
            samples[0].push(a);
            samples[1].push(b);
        }
    }
    let medians = samples.map(|mut times| {
        times.sort_unstable();
        (times[9] + times[10]) as f64 / (2.0 * 256.0 * scalars as f64)
    });
    println!("{name} {:.2} {:.2}", medians[0], medians[1]);
}

fn main() {
    println!("workload ICU_ns/scalar tables_ns/scalar");
    for (name, piece) in [
        ("ascii", "Orbital mechanics and astronomy 123 "),
        ("latin", "Crème brûlée, Ångström, Straße et l’Œuvre "),
        ("cjk", "東京大学대한민국上海轨道交通𠀟研究カタカナ "),
        ("mixed", "नमस्ते ঋतु ΟΣ العربية עברית 𖵪 "),
        ("jamo", "ᄒ\u{20dd}ᅡ\u{301}ᆫ나 "),
        ("aliases", "車龜女年六栗﨑晴"),
    ] {
        let input = piece.repeat(16);
        compare(
            &format!("{name}/normalize"),
            || icu_accents(black_box(&input)),
            || unicode::remove_accents(black_box(&input)),
            input.chars().count(),
        );
        let chars: Vec<char> = input.chars().collect();
        compare(
            &format!("{name}/word"),
            || black_box(&chars).iter().filter(|&&c| icu_word(c)).count(),
            || {
                black_box(&chars)
                    .iter()
                    .filter(|&&c| unicode::is_wordchar(c))
                    .count()
            },
            chars.len(),
        );
    }
}
