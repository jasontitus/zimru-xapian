// SPDX-License-Identifier: GPL-3.0-or-later
fn main() {
    use std::{
        collections::hash_map::DefaultHasher,
        fs,
        hash::{Hash, Hasher},
        hint::black_box,
        time::Instant,
    };
    let dir = std::env::args().nth(1).unwrap();
    let mut files = fs::read_dir(dir)
        .unwrap()
        .map(|p| p.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    let token = CancellationToken::new();
    let corpus: Vec<_> = files
        .into_iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect();
    let out = std::env::args().nth(2).unwrap();
    let mut h = DefaultHasher::new();
    let mut sizes = Vec::new();
    for (index, source) in corpus.iter().enumerate() {
        let output = sanitize(source, 390, &token).unwrap();
        fs::write(format!("{out}/{index}.html"), output.as_bytes()).unwrap();
        sizes.push(output.len());
        output.hash(&mut h);
    }
    let start = Instant::now();
    for _ in 0..12 {
        for source in &corpus {
            black_box(sanitize(black_box(source), 390, &token).unwrap());
        }
    }
    println!(
        "{} {:?} {}",
        h.finish(),
        sizes,
        start.elapsed().as_nanos() / 12
    );
}
