# Release CLI benchmark

The reviewed Rust reader reduced median peak RSS for a title query matching 50,000 articles from 13.00 MiB to 6.12 MiB. Median elapsed time fell from 163.95 ms to 112.77 ms. The numeric prefix spanning 10,000 distinct terms fell from 4.25 ms to 3.37 ms and used 920 KiB less peak memory. The four real-archive queries took 2.08–3.47 ms on the reviewed build. Their median peak RSS increased by 4–108 KiB.

## Measurements

Each comparison used release builds with the same locked dependencies, Rust 1.100.0, GNU time, and a shared x86-64 Linux container on AMD EPYC 9V74 hardware with a four-core CPU quota and 16 GiB memory limit. The baseline is commit `54b39d4`. The timed reader preceded the final whitespace-only query fast path. All timed queries contained non-whitespace text, and all 396 JSON outputs were rechecked with the final reader. Each workload received one warm-up per binary, followed by 11 measurements per binary with alternating run order. Median elapsed time includes starting a fresh CLI process under GNU time. Parentheses show the 3rd and 9th ordered times of the 11 samples. Peak RSS is the median of the process maxima.

| Workload | Baseline ms (middle 7) | Reviewed ms (middle 7) | Baseline peak MiB | Reviewed peak MiB |
| --- | ---: | ---: | ---: | ---: |
| synthetic-common | 5.03 (4.50–5.25) | 3.84 (3.67–4.11) | 6.06 | 4.36 |
| synthetic-rare | 2.84 (2.74–3.06) | 2.70 (2.65–2.83) | 4.35 | 4.39 |
| synthetic-and | 3.23 (3.02–3.60) | 3.17 (3.13–3.27) | 4.48 | 4.52 |
| synthetic-title | 163.95 (160.68–181.00) | 112.77 (109.55–125.10) | 13.00 | 6.12 |
| synthetic-prefix | 4.25 (4.15–4.62) | 3.37 (3.16–3.86) | 5.27 | 4.37 |
| climate-search | 3.94 (2.64–6.89) | 2.57 (2.25–6.20) | 2.96 | 3.03 |
| climate-title | 3.42 (3.28–3.68) | 3.47 (3.17–3.62) | 3.21 | 3.29 |
| nollywood-search | 2.28 (2.10–2.38) | 2.08 (1.98–2.26) | 2.71 | 2.71 |
| nollywood-title | 2.78 (2.69–3.10) | 2.77 (2.63–3.08) | 2.85 | 2.96 |
| write-fulltext | 450.79 (339.73–479.80) | 419.39 (359.13–500.43) | 21.07 | 20.78 |
| write-title | 320.16 (314.55–357.72) | 320.68 (309.84–352.38) | 43.95 | 44.03 |

The synthetic archive contains 50,000 distinct HTML articles. Every article contains the common term and the title prefix, and 50 contain the rare term. Each reader returned its top 10 on the same archive. The writers read the same 50,000 JSONL records and produced a full-text index of 3,629,056 bytes and a title index of 6,709,248 bytes in both builds. The title writer’s median peak RSS increased by 84 KiB. Writer time ranges overlap across builds. These measurements cover warm-file-cache, single-process Linux CLI workloads.

## Compatibility

The baseline and reviewed binaries returned identical JSON output, including scores, percentages, word counts and paths, for 396 invocations across the synthetic, Climate and Nollywood archives. These comprised 132 searches and suggestions at result limits 1, 10 and 30. On each build, all 120 libzim golden cases matched the ordered top-10 paths. The system libzim search and suggestion reader independently agreed on the synthetic common, rare, intersection and common-title top-10 results. On the 50,000-record writer inputs, Xapian 2.0.0 decoded identical metadata, document records, declared values, terms, posting lists and positions from both writers, using the native builder’s `bench/upstream-helper.cc snapshot` command for each database.

A fifth synthetic query exposed an inherited difference. The numeric title prefix `000` expands to 10,000 equally frequent terms. Both Rust builds return lexicographically earlier terms at the 100-term cutoff. Native Xapian can retain a different subset when frequencies tie. The native and Rust lists agreed at `00000`, which expands to exactly 100 terms. [Native expansion algorithm](https://github.com/xapian/xapian/blob/v2.0.0/xapian-core/api/queryinternal.cc#L286)

## Reproduction

Build zimru's `zimwriterfs` from the same revision as the dependency in Cargo.lock, and put the executable on PATH. Use a fresh temporary directory for the baseline checkout and corpus. The real-archive test downloads and verifies the Climate and Nollywood fixtures. GNU time is available as `gtime` on some systems. Its path can be set with `GNU_TIME`.

```sh
bench_dir=$(mktemp -d)
git worktree add --detach "$bench_dir/baseline" 54b39d4
cargo build --release --locked --bins --manifest-path "$bench_dir/baseline/Cargo.toml"
cargo build --release --locked --bins
ZIM_TESTDATA="$bench_dir/real" python3 tests/real_archives.py

python3 tests/performance.py prepare \
  --zimwriterfs "$(command -v zimwriterfs)" \
  --builder "$bench_dir/baseline/target/release/xapianbuilder" \
  --work "$bench_dir/corpus"

python3 tests/performance.py measure --runs 11 \
  --work "$bench_dir/corpus" --real "$bench_dir/real" \
  --baseline-search "$bench_dir/baseline/target/release/zimsearch" \
  --baseline-builder "$bench_dir/baseline/target/release/xapianbuilder" \
  --search target/release/zimsearch --builder target/release/xapianbuilder
```

The measurement command prints one JSON object per workload and binary with raw wall times, peak RSS samples, and a result digest or index size.

## Cache size on 64 KiB Glass blocks

On Apple Silicon macOS, the parent revision `d051622` cached 256 blocks of up to 64 KiB each. The revised cache retains at most 2 MiB of payload per open index. A regression measures retained block lengths at all six supported block sizes. Separate live cursors and decoder allocations can use additional memory.

A native Xapian 1.4.32 database held one document and 4,096 metadata keys named `key0` through `key4095`. Each value contained the low bytes of 4,096 successive C++ `mt19937` outputs, seeded once with 123. Native compaction produced an 18,219,008-byte, 64 KiB-block single-file Glass database. A release Rust process read all keys in numeric order twice using file-backed positional reads. Its accumulated value bytes matched on both builds. Seven warm-cache process launches per build alternated order after one warm-up pair. Process RSS came from Darwin `wait4`.

| Measurement | Parent revision | Bounded cache |
| --- | ---: | ---: |
| Median peak process RSS | 26.25 MiB | 6.20 MiB |
| Median elapsed time including startup | 19.49 ms | 18.26 ms |
| Source reads per run | 1,106 | 1,211 |

These measurements cover repeated sequential metadata queries on this generated index. The smaller cache reads additional blocks after eviction.
