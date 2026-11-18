# Opt-in level-19 archive writer

The `experimental-zstd-tree` feature uses a bounded binary tree to find repeated bytes when writing a Zstandard archive at level 19. Explicit selection changes the archive writer's match choices. The default writer, generic codec calls, dictionary frames, and parallel codec APIs retain their existing match finder. This feature affects archive creation. The Kiwix Tauri reader uses the separately pinned reader-only build.

```sh
cargo build --release --locked --features experimental-zstd-tree --bin zimrecreate
target/release/zimrecreate input.zim output.zim --compression zstd --compression-level 19 -j -J 4
```

Supply the separate GPL title-index builder with `--xapianbuilder-path` when using `-j`. Use `--without-indexes` for a codec-only comparison. Zimru's archive implementation retains its MIT license. The vendored codec retains its upstream licenses.

## What was compared

The end-to-end native control was upstream zim-tools 3.6.0 with libzim 9.8.2. Its `zimrecreate input.zim output.zim -j -J N` builds the title index and omits full text. This upstream command does not expose the compression level. The Rust command used the same input with explicit level 19 and the external GPL Glass builder. Both commands therefore include archive reads, title indexing, compression and archive finalization. Archive layout and title-index size can differ between implementations.

Each reported process run used a fresh output, with checks outside the timed interval. The complete source and output content-entry counts and framed content digests matched. On Mac, libzim-backed `zimcheck -I -C` passed and the native dumper extracted a Glass title index from every output. For three title queries per archive, the native suggestion API returned the same ten ordered paths from both writers. On Linux, libzim validated the structure and checksum and confirmed the presence of a title index and absence of a full-text index. All comparisons used existing or previously read input files. CPU is user plus system time reported for the exited CLI. Peak RSS is the OS process measurement, which does not sum simultaneous memory use across child processes. Host load varied.

## Compression tradeoffs

A valid archive carrying a 274,520-byte mixed-repeat body grew from 132,912 to 132,942 bytes when recreated with the experiment. A second archive with 16,013 lines of temperature data grew from 68,953 to 68,968 bytes. Creating archives from directories also grew slightly on both inputs. Native libzim recovered the exact original bytes from all 16 outputs at one and four workers. Recreating the same input archive with the default reproduced the existing writer's file bytes. New-from-directory builds can generate a new archive UUID. Independent tests also checked ordinary compression, explicit dictionaries and parallel frames at two configured windows. All 24 ordinary-API comparisons matched the existing compressed bytes and decoded with native Zstandard. These observations are why the feature is opt-in.

The bounded tree uses the existing chain table's two slots per node. The reviewed second-pass fix keeps position tokens representable when a public job starts well into a shared input near a generation rollover. The regression checks a real high-offset job and decodes its output. [Level-3 history tests and baseline](level-three-frame-history.md) · [Earlier isolated codec and large-input measurements](https://github.com/jasontitus/zimru-xapian/blob/0a44dcf3740cbe4466903d02e98bb87fd0c189db/docs/writer-benchmark-20261118.md)

At the combined `df695f0` source, a fresh opt-in build wrote a sparse 1,073,872,919-byte item at level 19 with four requested workers. Native libzim verified the archive and extracted the full item with the source SHA-256. The archive was 161,871 bytes and the writer used 185.2 MiB peak RSS. The fixture contains mostly zeros with scattered distinct records, so its throughput is not representative of article text. The reader-only, default, all-feature and vendored codec suites passed 117, 191, 200 and 56 checks respectively, including doctests, with the pinned archive fixtures present. [Large-item sample](review-data/level-19-large-item.csv)

## Whole-archive results

Across the ten measured Mac and Linux archive/worker cases, the opt-in writer had lower median elapsed time in nine. In single-worker Nollywood on Mac it took 60.52 s against 53.00 s upstream. Reported peak RSS was lower in all ten. The Rust output was smaller for Chemistry and larger for Climate and Nollywood. These are whole-archive differences, including the separately produced title index.

The table gives medians of three fresh processes per command and case. Native is upstream zim-tools. Rust is the opt-in level-19 writer. Order alternated on Linux and rotated on macOS. On macOS, the source was the opt-in revision `a77aa3f`. Linux also included the historical MIME preservation change in `df695f0`. The Mac used rustc 1.99.1 on Apple silicon, macOS 27.2.1 and native libzstd 1.5.7. The x86-64 Linux host used rustc 1.100.0 and native libzstd 1.5.5. Both used the standalone GPL builder at `380d84d`. [Mac process samples](review-data/level-19-upstream-macos.csv) · [Linux process samples](review-data/level-19-upstream-linux.csv)

| Platform | Archive | Workers | Native / Rust elapsed (s) | Native / Rust CPU (s) | Native / Rust peak RSS (MiB) | Native / Rust size (bytes) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| macOS ARM64 | Chinese Chemistry | 1 | 30.33 / 17.32 | 30.08 / 16.88 | 187.7 / 97.9 | 9,804,929 / 9,537,583 |
| macOS ARM64 | Chinese Chemistry | 4 | 10.98 / 6.07 | 24.89 / 16.61 | 441.0 / 238.5 | 9,804,929 / 9,537,583 |
| macOS ARM64 | Climate | 1 | 13.00 / 11.60 | 9.94 / 8.69 | 173.4 / 136.6 | 10,294,981 / 10,471,208 |
| macOS ARM64 | Climate | 4 | 4.99 / 3.76 | 9.71 / 7.46 | 422.9 / 302.7 | 10,294,981 / 10,471,208 |
| macOS ARM64 | Nollywood | 1 | 53.00 / 60.52 | 40.79 / 43.12 | 185.9 / 135.9 | 19,962,057 / 20,082,917 |
| macOS ARM64 | Nollywood | 4 | 13.26 / 11.38 | 50.66 / 43.73 | 435.1 / 285.6 | 19,962,057 / 20,082,917 |
| Linux x86-64 | Climate | 1 | 6.58 / 4.52 | 7.20 / 4.74 | 169.8 / 78.9 | 10,294,318 / 10,471,208 |
| Linux x86-64 | Climate | 4 | 2.31 / 1.68 | 9.04 / 5.93 | 409.0 / 222.4 | 10,294,318 / 10,471,208 |
| Linux x86-64 | Chinese Chemistry | 1 | 13.81 / 9.21 | 14.24 / 9.43 | 185.4 / 62.1 | 9,796,000 / 9,537,583 |
| Linux x86-64 | Chinese Chemistry | 4 | 4.61 / 2.90 | 18.18 / 10.96 | 435.4 / 192.8 | 9,796,000 / 9,537,583 |

The source SHA-256 values for Chemistry, Climate and Nollywood are recorded in the [level-3 study](level-three-frame-history.md). The Wikipedia 100 input on Linux had SHA-256 `bde9d73bb1a6b959af6cf4f00ed8f6aac6cf7f4392e28a16a46cd77228b16613`. Zimru retained all 8,413 content entries, including the 18 entries labeled with the historical MIME value `undefined`. Upstream zim-tools recreated a structurally valid archive but changed the user-content digest. Native libzim found 101 article bodies differed. The comparison excluded that archive from its timings.

To reproduce, build Zimru with the command above and the GPL companion in its own checkout. For each archive and worker count (1 or 4), run the native and Rust writer commands given above into separate temporary files. Use the same input and a fresh process for each run. Record elapsed time, child process CPU, peak RSS and file size. Compare `zimru readall --content-md5` on each output and its input, run native `zimcheck -I -C`, and use `zimdump show --ns=X --url=title/xapian` to check the rebuilt title index. A mobile writer was outside this study.
