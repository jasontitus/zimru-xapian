# Level-3 Zstandard writer history

A one-worker Zimru writer could declare an 8 MiB history window but search only the last 2 MiB at level 3. Two copies of the same 3 MiB of real archive content therefore compressed almost as two separate inputs. The writer now uses its configured frame window for this case. It retains the existing job and prefix budgets. Explicit codec level 0 selects level 3. The archive writer defaults to level 19.

## Repeated real content

Sizes in bytes are medians of five measured runs after one warm-up. Each frame contains two or four copies of the first 3 MiB of consecutive native-decoded Zstandard cluster payloads from the named archive. All 200 measured frames were decoded with native Zstandard and checked against the expected SHA-256. Each fixture, decoded payload hash, process CPU time, elapsed encoding time and peak RSS is in the [frame samples](review-data/level-three-frames.csv).

| Archive | Input | Native default | Native 8 MiB window | Rust before | Rust after |
| --- | ---: | ---: | ---: | ---: | ---: |
| Climate | 6 MiB | 716,882 | 360,481 | 719,349 | 363,162 |
| Climate | 12 MiB | 1,425,152 | 361,082 | 1,427,035 | 363,798 |
| Nollywood | 6 MiB | 774,167 | 390,808 | 775,446 | 393,803 |
| Nollywood | 12 MiB | 1,535,801 | 391,385 | 1,537,143 | 394,724 |

The original reported twofold gap used an explicitly enlarged native window. The 6 MiB Rust frames are now within 0.8% of that reference size. Median process CPU on Climate was 60.5 ms before, 39.3 ms after and 27.0 ms native with the enlarged window. On Nollywood the corresponding times were 55.0, 35.1 and 27.3 ms. Peak RSS for Rust fell from 15.5 to 13.6 MiB on Climate and from 15.4 to 13.4 MiB on Nollywood. Native used 12.4 and 12.3 MiB respectively. Four-worker level 3 still uses smaller independent jobs and can miss these repeats. The four-worker measurements and a reconstruction of the earlier 2 MiB job policy are included in the samples.

## Whole archives

Three interleaved process runs per build and worker count recreated each archive at level 3 without indexes. The native control is Zimru revision `849aa41605065a82e5b0dd998d741f0f8c213efe` using libzstd 1.5.7. The Rust baseline is `ff8b0ab5e561b0019151d6d614131f59920ff2ee`. For all 54 outputs the control reader reproduced the source's full content digest and entry count, and libzim-backed `zimcheck -I -C` passed. These whole-archive timings compare codec implementations through the Zimru CLI. Measurements were taken on a shared Apple silicon Mac with rustc 1.99.1. Timing includes reading, decoding and writing. [Individual archive measurements](review-data/level-three-archives.csv)

| Archive | Workers | Native / before / after CPU (s) | Native / before / after peak RSS (MiB) | Native / after output (bytes) |
| --- | ---: | ---: | ---: | ---: |
| Chinese Chemistry | 1 | 0.378 / 0.641 / 0.578 | 70.0 / 69.3 / 66.2 | 8,824,549 / 8,847,194 |
| Chinese Chemistry | 4 | 0.400 / 0.596 / 0.600 | 69.7 / 85.2 / 88.5 | 8,824,549 / 8,847,194 |
| Climate | 1 | 0.305 / 0.425 / 0.419 | 66.0 / 58.8 / 59.7 | 7,233,293 / 7,256,193 |
| Climate | 4 | 0.310 / 0.431 / 0.407 | 71.0 / 67.6 / 73.1 | 7,233,293 / 7,256,193 |
| Nollywood | 1 | 0.682 / 1.256 / 0.948 | 102.1 / 87.1 / 85.8 | 22,097,948 / 22,197,055 |
| Nollywood | 4 | 0.808 / 1.236 / 1.272 | 112.3 / 109.1 / 112.2 | 22,097,948 / 22,197,055 |

Each whole-archive Rust output had exactly the same size before and after. Native was faster in all six elapsed-time medians. Small clusters and the four-worker path limit how often the larger history can help. These results establish a size improvement for long-distance repetitions. They do not establish a general throughput improvement. The decoder and level-19 match-finder experiment are separate from this change.

## Reproduction

Use a separate Cargo target directory per source checkout. Build the above native and baseline revisions and this candidate with `cargo build --release --locked --bin zimru --bin zimrecreate`. Use the source file names and SHA-256 values in the frame samples for Climate and Nollywood. The Chemistry file is `wikipedia_zh_chemistry_mini.zim`, SHA-256 `47849c47e10929b55c954a15691191fa1d5ce07570d787e93d9a1d4cfba3933b`.

```sh
"$build/zimrecreate" "$archive" "$temporary_output" --without-indexes \
  --compression zstd --compression-level 3 --threads "$workers"
"$native/zimru" readall "$archive" --content-md5
"$native/zimru" readall "$temporary_output" --content-md5
zimcheck -I -C "$temporary_output"
```

For the frame test, unpack the [existing writer harness](review-data/zstd-writer-experiment.tar.gz) next to the source checkout named `zimru`. Run its `extract.py` with `ZIM_FIXTURE_DIR` set to the two archives. Concatenate decoded clusters for one archive in numeric cluster order and take exactly the first 3,145,728 bytes as `source.bin`. Build the harness in release mode against each checkout. Run `writer-codec-check rust 3 1 source.bin 2 result.zst`, then repeat with four copies and with four requested workers. Use its `native` mode for the default control. For the enlarged native control, call `writer.window_log(23).unwrap()` after setting the pledged size in the native harness. For the historical job policy control use the baseline encoder with `long_window = level >= 16 && size > JOB_BYTES as u64`. Interleave engines, discard one warm-up, and compare `zstd -dqc result.zst` with the expected repeated bytes each time. Native code serves as an external reference and test dependency. The production encoder uses the vendored Rust codec.
