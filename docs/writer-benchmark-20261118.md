# Zstandard writer experiment on macOS

The comparison uses the MIT archive project at `ca5e62e965e0667120319906af2aee9791dc94fb`, its native-codec predecessor at `849aa41605065a82e5b0dd998d741f0f8c213efe`, and the Rust revision with the archived binary-tree match-finder experiment applied. The patch SHA-256 is `a7a1c5f5db787b6e0e3516f5c8101c347df0d1d9c5a778f5fb219d169dc17abd`. The production codec stays at its reviewed revision. [Experiment and provenance](rust-codecs.md)

At level 19 the tree used less process CPU than the production Rust writer in all six archive/worker combinations and improved its output size on all three archives. Chemistry and Climate also beat native throughput in those conditions. Single-worker Nollywood reached 4.9 MiB/s versus 5.3 MiB/s natively, and level 3 remained slower than native across these archives. The measurements support further high-level encoder work. They leave a general native-performance or file-size parity claim unsupported. The patch remains an experiment.

## Measurement

On arm64 macOS 27.2.1, all three builds used Homebrew Rust 1.99.1, release settings and the committed dependency locks. Native verification used zim-tools 3.6.0, libzim 9.8.2 and libzstd 1.5.7. The host ran other work during this test. The full-archive table uses medians of three new processes per condition. Build order rotated each round and across archive/worker combinations. Verification primed each input, and every round is included. Results include archive reading, content transfer, compression, archive finalization and process startup. The `--threads` value requests the archive worker count. For one large level-19 input, the Rust encoder uses at most two concurrent codec jobs, even when four archive workers are requested.

Each timed archive rebuild omitted indexes. A separate native-codec MIT reader compared its complete user-entry count and length-framed logical digest with the input, covering paths, titles, redirects, MIME types and blob contents. Separately, native libzim's `zimcheck -I -C` verified structure and checksum for every output. The input content sizes below exclude indexes, metadata and redirects, using `zimdump list --details` item sizes. MiB/s divides those bytes by elapsed time. CPU sums user and system time from `wait4` for the writer process, including worker threads. Peak RSS is the writer process high-water mark. Native verification ran outside all timed intervals.

The columns list native / production Rust / experimental Rust in that order. Each size delta compares a Rust output with native. [All 108 samples](review-data/writer-macos-20261118/whole.csv)

| Archive | Level | Workers | MiB/s | CPU s | Peak RSS MiB | Size delta, production / experiment |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Chemistry | 3 | 1 | 524.7 / 374.9 / 383.1 | 0.19 / 0.25 / 0.25 | 69 / 60 / 52 | +0.26% / +0.26% |
| Chemistry | 3 | 4 | 744.9 / 597.6 / 580.9 | 0.17 / 0.25 / 0.26 | 57 / 55 / 61 | +0.26% / +0.26% |
| Climate | 3 | 1 | 252.5 / 203.8 / 202.2 | 0.13 / 0.18 / 0.18 | 47 / 51 / 53 | +0.32% / +0.32% |
| Climate | 3 | 4 | 297.6 / 260.0 / 291.0 | 0.14 / 0.18 / 0.18 | 59 / 60 / 61 | +0.32% / +0.32% |
| Nollywood | 3 | 1 | 480.2 / 304.5 / 307.1 | 0.27 / 0.43 / 0.43 | 70 / 74 / 58 | +0.45% / +0.45% |
| Nollywood | 3 | 4 | 849.3 / 634.0 / 621.2 | 0.28 / 0.44 / 0.45 | 69 / 70 / 84 | +0.45% / +0.45% |
| Chemistry | 19 | 1 | 8.1 / 5.5 / 11.2 | 8.53 / 12.94 / 6.41 | 91 / 86 / 82 | +0.79% / +0.44% |
| Chemistry | 19 | 4 | 30.6 / 19.5 / 39.5 | 8.65 / 13.63 / 6.70 | 282 / 225 / 219 | +0.79% / +0.44% |
| Climate | 19 | 1 | 6.2 / 4.5 / 9.4 | 4.87 / 6.73 / 3.23 | 141 / 122 / 124 | +0.56% / +0.19% |
| Climate | 19 | 4 | 21.2 / 12.7 / 31.1 | 5.00 / 8.34 / 3.38 | 288 / 287 / 285 | +0.56% / +0.19% |
| Nollywood | 19 | 1 | 5.3 / 3.1 / 4.9 | 19.50 / 32.98 / 20.67 | 261 / 130 / 141 | +0.69% / +0.15% |
| Nollywood | 19 | 4 | 19.1 / 9.8 / 19.7 | 20.62 / 40.53 / 20.07 | 439 / 280 / 277 | +0.69% / +0.15% |

## History across compression jobs

The Climate frame test uses 12 MiB of consecutive native-decoded Zstandard cluster bytes, or the same first 3 MiB repeated four times. Both inputs cross the high-level encoder's 8 MiB job boundary. The repeated history fits within a high-level job's 4 MiB overlap. Every resulting frame was decoded by native Zstandard and checked against the complete source SHA-256. These are single-process samples per condition. [Frame samples](review-data/writer-macos-20261118/history.csv)

| Pattern | Level | Workers | CPU s, native / production / experiment | Peak RSS MiB, native / production / experiment | Experimental frame size vs native |
| --- | ---: | ---: | ---: | ---: | ---: |
| Adjacent | 3 | 1 | 0.04 / 0.04 / 0.04 | 17 / 27 / 27 | -0.28% |
| Adjacent | 3 | 4 | 0.02 / 0.04 / 0.04 | 30 / 30 / 30 | +1.28% |
| Adjacent | 19 | 1 | 3.01 / 4.23 / 1.74 | 103 / 106 / 106 | +1.89% |
| Adjacent | 19 | 4 | 3.14 / 5.14 / 1.87 | 108 / 192 / 192 | +1.89% |
| Repeat | 3 | 1 | 0.04 / 0.04 / 0.04 | 8 / 18 / 18 | +0.13% |
| Repeat | 3 | 4 | 0.02 / 0.04 / 0.04 | 21 / 22 / 22 | +0.95% |
| Repeat | 19 | 1 | 0.63 / 0.76 / 0.47 | 94 / 95 / 95 | +0.36% |
| Repeat | 19 | 4 | 0.63 / 0.91 / 0.49 | 99 / 180 / 180 | +0.31% |

At level 19 the tree reduced the adjacent-text frame from 958,650 to 934,883 bytes. Native produced 917,562 bytes with one worker. This 1.89% size difference limits a size-parity claim on long article text.

## Streamed item and coverage

The experimental command-line writer compressed a sparse 1,073,872,919-byte text item at level 19 with four requested workers. The full archive was 161,871 bytes and the writer's peak RSS was 185.2 MiB. Native libzim passed structural/checksum validation and extracted exactly 1,073,872,919 bytes with the source SHA-256 `58f057c94fb1c8ba1b536096729ccc7942ae871b3b48af66e10f009f756322e0`. Most of this item consists of zeros, interspersed with short distinct records every 2 MiB. It exercises streamed frame boundaries and finalization. Its compression size and throughput do not predict performance on a gigabyte of ordinary article text. [Stream sample](review-data/writer-macos-20261118/large.csv)

Source review followed `Compressor::blocks`, `OptState::parse` and the experimental `tree::insert` from frame setup through prefix preload, ring wrap and block transitions. The byte comparison stops at the current block end. Table generations, window distance and the ordered-node test restrict references to earlier valid bytes. Reusing chain memory for two links halves its number of addressable nodes. Dictionary-aware parsing continues through the existing matcher when a dictionary is attached. The selected upstream suite passed 49 tests and 3 doctests. The archive streaming and writer roundtrip suites passed 21 tests on the patched source. These checks cover the encoder and command-line writer on the measured host. The reader, mobile hardware, concurrent write failures under low disk space and input distributions beyond these archives require their own evidence.

## Reproduction

Use an empty scratch directory with paths to the three original archives. Verify their SHA-256 digests against [input identities](review-data/writer-macos-20261118/inputs.json). The archived experiment's `fixtures.tsv` identifies the two English sources. The Chinese Chemistry sample was supplied locally. The input archives stay external to these checkouts. Install a native C/C++ toolchain, Python 3.9+, Rust and the native `zstd`, `zimcheck` and `zimdump` verification tools. Confirm `zimcheck -V` and `zimdump --version` both load the intended libzim before starting.

From this repository checkout, create three isolated worktrees in the scratch directory. Substitute your absolute file paths for the three archive arguments.

```sh
bench_repo=$(pwd)
bench_lab=$(mktemp -d)
git worktree add --detach "$bench_lab/native" 849aa41605065a82e5b0dd998d741f0f8c213efe
git worktree add --detach "$bench_lab/stable" ca5e62e965e0667120319906af2aee9791dc94fb
git worktree add --detach "$bench_lab/tree" ca5e62e965e0667120319906af2aee9791dc94fb
mkdir "$bench_lab/experiment"
tar -xzf "$bench_lab/stable/docs/review-data/zstd-writer-experiment.tar.gz" -C "$bench_lab/experiment"
git -C "$bench_lab/tree" apply "$bench_lab/experiment/optimal-tree.patch"
for build in native stable tree; do
  cargo build --locked --release --manifest-path "$bench_lab/$build/Cargo.toml" --bin zimru --bin zimrecreate --bin zimwriterfs
done
cargo test --locked --release --manifest-path "$bench_lab/tree/vendor/zstd-rs/Cargo.toml"
cargo test --locked --release --manifest-path "$bench_lab/tree/Cargo.toml" --test streaming_encode_smoke --test writer_roundtrip
python3 "$bench_repo/bench/measure_writer.py" whole "$bench_lab/native/target/release" "$bench_lab/stable/target/release" "$bench_lab/tree/target/release" /path/to/chemistry.zim /path/to/climate.zim /path/to/nollywood.zim > "$bench_lab/whole.csv"
```

The bundled frame harness expects the MIT checkout at `../../zimru` relative to its manifest. Run each copy with its matching source tree.

```sh
for build in stable tree; do
  mkdir -p "$bench_lab/frame-$build/experiment"
  ln -s "$bench_lab/$build" "$bench_lab/frame-$build/zimru"
  cp -R "$bench_lab/experiment/harness" "$bench_lab/frame-$build/experiment/"
  cargo build --release --locked --manifest-path "$bench_lab/frame-$build/experiment/harness/Cargo.toml"
done
python3 "$bench_repo/bench/measure_writer.py" history "$bench_lab/frame-stable/experiment/harness/target/release/writer-codec-check" "$bench_lab/frame-tree/experiment/harness/target/release/writer-codec-check" /path/to/climate.zim > "$bench_lab/history.csv"
python3 "$bench_repo/bench/measure_writer.py" large "$bench_lab/tree/target/release/zimwriterfs" > "$bench_lab/large.csv"
```
