//! The reader against the real indexes of the climate archive (`just testdata`),
//! values decoded independently with `xapian-delve`.

use super::*;
use crate::testdata::{climate, index};

#[test]
fn version_block() {
    let a = climate();
    let ft = index(&a, true);
    assert_eq!(&ft.uuid[..4], &[0x62, 0x95, 0xd1, 0x47]);
    assert_eq!(ft.revision, 1);
    assert_eq!(ft.blocksize(), 8192);
    let pl = &ft.roots[db::POSTLIST];
    assert_eq!((pl.root, pl.level, pl.entries), (3, 1, 27034));
    assert!(ft.roots[db::POSITION].fake && ft.roots[2].fake);
    assert!(!ft.has_positions());
    let s = &ft.stats;
    assert_eq!(
        (s.doccount, s.last_docid, s.total_doclen),
        (3821, 3821, 797963)
    );
    assert_eq!(
        (s.doclen_lbound, s.doclen_ubound, s.wdf_ubound),
        (6, 1617, 105)
    );
    let t = index(&a, false);
    assert_eq!(t.stats.doccount, 3569);
    assert!(t.has_positions());
}

#[test]
fn docdata_metadata_terms() {
    let a = climate();
    let ft = index(&a, true);
    assert_eq!(
        ft.data(5).unwrap().as_deref(),
        Some(&b"C/Albert_Sustainable_Production_Certification"[..])
    );
    assert_eq!(ft.data(99999).unwrap(), None);
    assert_eq!(
        ft.metadata("language").unwrap().as_deref(),
        Some(&b"eng"[..])
    );
    assert_eq!(
        ft.metadata("kind").unwrap().as_deref(),
        Some(&b"fulltext"[..])
    );
    assert_eq!(ft.metadata("stopwords").unwrap(), None);
    assert_eq!(ft.freqs(b"climat").unwrap(), (2414, 10917));
    assert_eq!(ft.freqs(b"nosuchterm").unwrap(), (0, 0));
    let terms = ft.terms(b"climat").unwrap();
    assert_eq!(terms[0], (b"climat".to_vec(), 2414));
    assert!(terms.iter().all(|(t, _)| t.starts_with(b"climat")));
    assert!(terms.windows(2).all(|w| w[0].0 < w[1].0));
}

#[test]
fn postlists_and_doclens() {
    let a = climate();
    let ft = index(&a, true);
    // Every posting of "climat", walked and then skipped to.
    let mut pl = ft.postlist(b"climat").unwrap().unwrap();
    let mut all = vec![];
    while pl.next().unwrap() {
        all.push((pl.did(), pl.wdf()));
    }
    assert_eq!(all.len(), 2414);
    assert_eq!(all[0].0, 1);
    assert_eq!(all.iter().map(|x| x.1 as u64).sum::<u64>(), 10917);
    assert!(all.windows(2).all(|w| w[0].0 < w[1].0));
    for step in [1, 7, 100, 1000] {
        let mut pl = ft.postlist(b"climat").unwrap().unwrap();
        for &(did, wdf) in all.iter().step_by(step) {
            assert!(pl.skip_to(did).unwrap());
            assert_eq!((pl.did(), pl.wdf()), (did, wdf));
        }
        assert!(!pl.skip_to(all.last().unwrap().0 + 1).unwrap());
    }
    // Skipping to a docid between postings lands on the next one.
    let mut pl = ft.postlist(b"climat").unwrap().unwrap();
    let gap = all.windows(2).find(|w| w[1].0 > w[0].0 + 1).unwrap();
    assert!(pl.skip_to(gap[0].0 + 1).unwrap());
    assert_eq!(pl.did(), gap[1].0);
    // Document lengths: one per document, summing to total_doclen.
    let mut dl = ft.doclens().unwrap().unwrap();
    let (mut n, mut sum) = (0, 0u64);
    while dl.next().unwrap() {
        n += 1;
        sum += dl.wdf() as u64;
    }
    assert_eq!((n, sum), (3821, 797963));
    let mut dl = ft.doclens().unwrap().unwrap();
    assert!(dl.skip_to(3000).unwrap());
    assert_eq!(dl.did(), 3000);
}

#[test]
fn values_and_positions() {
    let a = climate();
    let ft = index(&a, true);
    assert_eq!(
        ft.value(5, 0).unwrap().as_deref(),
        Some(&b"albert sustainable production certification"[..])
    );
    let wc = ft.value(5, 1).unwrap().unwrap();
    assert!(String::from_utf8(wc).unwrap().parse::<u32>().unwrap() > 0);
    assert_eq!(ft.value(5, 9).unwrap(), None);
    assert_eq!(ft.value_stats(0).unwrap().unwrap().0, 3821);
    let t = index(&a, false);
    // "0posanchor" is at position 1 of every title document.
    for did in [1, 2, 100, 3569] {
        assert_eq!(t.positions(did, b"0posanchor").unwrap(), vec![1]);
    }
}

/// A tiny xorshift generator: deterministic "random" tests, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Write `b` and open it again; with `XAPIAN_CHECK` set (a `xapian-check`
/// binary), also check it with Xapian.
fn round_trip(b: &write::Builder, name: &str) -> Db {
    let mut out = std::io::Cursor::new(vec![]);
    b.write(&mut out, *b"0123456789abcdef").unwrap();
    let bytes = out.into_inner();
    assert_eq!(bytes.len() % write::BLOCKSIZE, 0);
    let path = std::env::temp_dir().join(format!("zimfile-{name}.glass"));
    std::fs::write(&path, &bytes).unwrap();
    if let Ok(check) = std::env::var("XAPIAN_CHECK") {
        let o = std::process::Command::new(check)
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&o.stdout);
        assert!(
            o.status.success() && text.contains("No errors found"),
            "{text}"
        );
    }
    Db::open(Box::new(bytes)).unwrap()
}

#[test]
fn write_round_trip() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut b = write::Builder::default();
    let vocab: Vec<Vec<u8>> = (0..3000)
        .map(|i| format!("w{}{}", i, "x".repeat(i % 7)).into_bytes())
        .chain([b"nul\0term".to_vec(), b"every".to_vec(), vec![0xff; 3]])
        .collect();
    let mut docs = vec![];
    for d in 0..2500u32 {
        let mut terms = write::Terms::new();
        let n = 1 + rng.below(30);
        let mut pos = 1;
        for _ in 0..n {
            let t = vocab[rng.below(vocab.len() as u64 - 2) as usize].clone();
            let e = terms.entry(t).or_insert((0, vec![]));
            e.0 += 1 + rng.below(3) as u32;
            pos += 1 + rng.below(5) as u32;
            e.1.push(pos);
        }
        terms.insert(b"every".to_vec(), (1, vec![]));
        // Many positions: (k² + d) for k in 1..=(d % 60 + 2).
        let many: Vec<u32> = (1..=d % 60 + 2).map(|k| k * k + d).collect();
        terms.insert(b"many".to_vec(), (many.len() as u32, many));
        let data = if d % 100 == 7 {
            vec![b'p'; 5000] // spans components
        } else {
            format!("C/doc{d}").into_bytes()
        };
        let mut values = vec![(0, format!("title {d}").into_bytes())];
        if d % 3 == 0 {
            values.push((1, d.to_string().into_bytes()));
        }
        values.push((2, vec![])); // empty: not stored
        assert_eq!(b.add(&data, &terms, &values), d + 1);
        docs.push((data, terms, values));
    }
    b.set_metadata("language", b"eng");
    b.set_metadata("big", &vec![b'm'; 50_000]);
    b.set_metadata("empty", b"");
    let db = round_trip(&b, "round-trip");

    assert_eq!(
        db.metadata("language").unwrap().as_deref(),
        Some(&b"eng"[..])
    );
    assert_eq!(db.metadata("big").unwrap().unwrap().len(), 50_000);
    assert_eq!(db.metadata("empty").unwrap(), None);
    assert_eq!(db.stats.doccount, 2500);
    assert_eq!(db.stats.last_docid, 2500);
    let lens: Vec<u32> = docs
        .iter()
        .map(|d| d.1.values().map(|x| x.0).sum())
        .collect();
    assert_eq!(
        db.stats.total_doclen,
        lens.iter().map(|&l| l as u64).sum::<u64>()
    );
    assert_eq!(db.stats.doclen_lbound, *lens.iter().min().unwrap());
    assert_eq!(db.stats.doclen_ubound, *lens.iter().max().unwrap());
    let wdf_ub = docs
        .iter()
        .flat_map(|d| d.1.values().map(|x| x.0))
        .max()
        .unwrap();
    assert_eq!(db.stats.wdf_ubound, wdf_ub);

    let mut dl = db.doclens().unwrap().unwrap();
    for &l in &lens {
        assert!(dl.next().unwrap());
        assert_eq!(dl.wdf(), l);
    }
    assert!(!dl.next().unwrap());

    // Inverted: every term's postings.
    let mut inv: std::collections::BTreeMap<Vec<u8>, Vec<(u32, u32)>> = Default::default();
    for (i, d) in docs.iter().enumerate() {
        for (t, (wdf, _)) in &d.1 {
            inv.entry(t.clone()).or_default().push((i as u32 + 1, *wdf));
        }
    }
    for (t, ps) in &inv {
        let cf: u64 = ps.iter().map(|p| p.1 as u64).sum();
        assert_eq!(db.freqs(t).unwrap(), (ps.len() as u32, cf));
        let mut pl = db.postlist(t).unwrap().unwrap();
        for p in ps {
            assert!(pl.next().unwrap());
            assert_eq!((pl.did(), pl.wdf()), *p);
        }
        assert!(!pl.next().unwrap());
        let mut pl = db.postlist(t).unwrap().unwrap();
        for p in ps.iter().step_by(3) {
            assert!(pl.skip_to(p.0).unwrap());
            assert_eq!((pl.did(), pl.wdf()), *p);
        }
    }
    let listed: Vec<Vec<u8>> = db.terms(b"").unwrap().into_iter().map(|t| t.0).collect();
    assert_eq!(listed, inv.keys().cloned().collect::<Vec<_>>());
    assert_eq!(
        db.terms(b"nul").unwrap(),
        vec![(
            b"nul\0term".to_vec(),
            inv[&b"nul\0term".to_vec()].len() as u32
        )]
    );

    for (i, d) in docs.iter().enumerate() {
        let did = i as u32 + 1;
        assert_eq!(db.data(did).unwrap().unwrap(), d.0);
        for (slot, v) in &d.2 {
            let want = (!v.is_empty()).then(|| v.clone());
            assert_eq!(db.value(did, *slot).unwrap(), want);
        }
        if did % 3 != 1 {
            assert_eq!(db.value(did, 1).unwrap(), None);
        }
        for (t, (_, pos)) in &d.1 {
            assert_eq!(&db.positions(did, t).unwrap(), pos);
        }
    }
    let (freq, lo, hi) = db.value_stats(0).unwrap().unwrap();
    assert_eq!(
        (freq, &lo[..], &hi[..]),
        (2500, &b"title 0"[..], &b"title 999"[..])
    );
    assert_eq!(db.value_stats(1).unwrap().unwrap().0, 834);
    assert_eq!(db.value_stats(2).unwrap(), None);
}

#[test]
fn write_deep_tree() {
    // Long keys: ~1300 leaves and a level-2 root.
    let mut b = write::Builder::default();
    for d in 0..100_000u32 {
        let mut terms = write::Terms::new();
        terms.insert(format!("{d:0>100}").into_bytes(), (1, vec![]));
        b.add(b"", &terms, &[]);
    }
    let db = round_trip(&b, "deep");
    assert!(
        db.roots[db::POSTLIST].level >= 2,
        "{:?}",
        db.roots[db::POSTLIST]
    );
    assert!(db.roots[db::DOCDATA].fake);
    for d in (0..100_000u32).step_by(997) {
        let t = format!("{d:0>100}");
        let mut pl = db.postlist(t.as_bytes()).unwrap().unwrap();
        assert!(pl.next().unwrap());
        assert_eq!(pl.did(), d + 1);
    }
    assert_eq!(db.terms(b"0000").unwrap().len(), 100_000);
}
