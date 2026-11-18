use std::io::{self, Write};
use zimru::Archive;

fn main() {
    let archive = Archive::open(std::env::args_os().nth(1).unwrap()).unwrap();
    let mut out = io::BufWriter::new(io::stdout().lock());
    for i in 0..archive.cluster_count() {
        let cluster = archive.cluster_uncached(i).unwrap();
        out.write_all(&cluster.blob_count().to_le_bytes()).unwrap();
        for j in 0..cluster.blob_count() {
            let blob = cluster.blob(j).unwrap();
            out.write_all(&(blob.len() as u64).to_le_bytes()).unwrap();
            out.write_all(blob).unwrap();
        }
    }
    out.flush().unwrap();
}
