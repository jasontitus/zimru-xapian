//! Time a full pass through the public uncached cluster reader.
use std::time::Instant;
use zimru::Archive;

fn main() {
    let path = std::env::args_os().nth(1).expect("archive path");
    let archive = Archive::open(path).unwrap();
    let start = Instant::now();
    let mut total = 0u64;
    for i in 0..archive.cluster_count() {
        let cluster = archive.cluster_uncached(i).unwrap();
        for b in 0..cluster.blob_count() {
            total += cluster.blob(b).unwrap().len() as u64;
        }
    }
    println!("{} {total}", start.elapsed().as_nanos());
}
