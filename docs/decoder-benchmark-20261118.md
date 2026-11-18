# Zstandard decoder experiment on Apple Silicon

The production reader uses the vendored Avarok Zstandard decoder at MIT revision `ca5e62e965e0667120319906af2aee9791dc94fb`. The experimental patch keeps unread bits left-aligned and reloads the second sequence window only when the three extra fields could leave too little room for the FSE state transitions. A 64-bit refill supplies at least 57 bits away from the physical start. The three transitions consume at most 26 bits, so a combined extra width of up to 31 fits in that window. Full-width terminal reads, zero-padding and exact final bit consumption remain checked. The new regression compares reads, peeks and end states with an independent bit-by-bit calculation. [Patch](review-data/zstd-decoder-macos-20261118/reader.patch)

## Host comparison

On an Apple M1 Ultra running macOS 27.2.1, all three revisions used Homebrew rustc 1.99.1 and their committed dependency locks in release mode. Each fresh process opened the same archive and read all clusters through `Archive::cluster_uncached`. The measured loop visited every blob and summed its byte length. Each archive had one discarded warm-up per build and 25 measured samples per build in rotating order. Input files were in the operating system cache. Other processes shared the machine.

Wall time below covers the cluster loop. CPU includes both user and system time over the full process. Peak RSS uses the full process high-water mark. Each cell gives independent medians for the native-codec MIT reference, production Rust, and experimental Rust, in that order. The native-codec reference is `849aa41605065a82e5b0dd998d741f0f8c213efe`. [Raw samples with input and executable SHA-256](review-data/zstd-decoder-macos-20261118/samples.json)

| Archive | Cluster-loop wall ms | Process CPU ms | Peak RSS MiB |
| --- | ---: | ---: | ---: |
| Chemistry | 29.16 / 46.34 / 43.49 | 32.50 / 49.29 / 46.39 | 42.22 / 30.31 / 30.11 |
| Climate | 13.03 / 21.34 / 19.84 | 16.41 / 24.35 / 22.72 | 20.17 / 15.06 / 14.84 |
| Nollywood | 47.30 / 81.16 / 75.85 | 50.75 / 83.97 / 78.94 | 26.81 / 22.34 / 22.12 |

The experimental Rust reader took less median CPU than production on all three measured archives. The native-codec reference was faster on each. The patched vendor suite passed 50 tests and three doctests on arm64 macOS. Every cluster's blob count and every blob's length and payload matched the native-codec reader on all five checked archives, including two smaller Ray Charles archives. [Ray Charles verification](review-data/zstd-decoder-macos-20261118/ray-verification.json) The three timed input identities appear in the raw samples. This experiment covers warm, sequential archive decoding. It leaves Linux, 32-bit, cold storage and device behavior unmeasured. The production decoder remains the reviewed Avarok fork.

## Reproduce

From the private MIT checkout, provide three archive paths matching the SHA-256 identities in the data. The Chinese Chemistry archive was supplied locally. Climate and Nollywood are the English Kiwix fixtures used by the writer comparison. Use Rust, Python 3.9 or later, and the native development dependencies of the reference revision. The runner uses POSIX child resource accounting on Linux or macOS.

```sh
repo=$(pwd)
lab=$(mktemp -d)
evidence="$repo/docs/review-data/zstd-decoder-macos-20261118"
git worktree add --detach "$lab/native" 849aa41605065a82e5b0dd998d741f0f8c213efe
git worktree add --detach "$lab/production" ca5e62e965e0667120319906af2aee9791dc94fb
git worktree add --detach "$lab/experiment" ca5e62e965e0667120319906af2aee9791dc94fb
git -C "$lab/experiment" apply "$evidence/reader.patch"
cp "$lab/production/examples/codec_read.rs" "$lab/native/examples/codec_read.rs"
for build in native production experiment; do
  cp "$evidence/blob_manifest.rs" "$lab/$build/examples/blob_manifest.rs"
  cargo build --release --locked --target-dir "$lab/target-$build" \
    --manifest-path "$lab/$build/Cargo.toml" --example codec_read --example blob_manifest
done
cargo test --release --locked --manifest-path "$lab/experiment/vendor/zstd-rs/Cargo.toml"
python3 "$repo/bench/measure_decoder.py" \
  "$lab/target-native/release/examples" "$lab/target-production/release/examples" \
  "$lab/target-experiment/release/examples" /path/to/chemistry.zim /path/to/climate.zim \
  /path/to/nollywood.zim > "$lab/samples.json"
```

The runner first compares three independently emitted SHA-256 manifests. Each manifest frames cluster blob counts as little-endian u32 and each length as little-endian u64 before the exact bytes. Digest generation runs outside the timed scans. Keep separate Cargo targets for the three source trees so same-named binaries remain identifiable. [Measurement script](../bench/measure_decoder.py) · [Manifest example](review-data/zstd-decoder-macos-20261118/blob_manifest.rs)
