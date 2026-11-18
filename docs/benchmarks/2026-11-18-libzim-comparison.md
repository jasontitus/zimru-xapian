# Kiwix and Zimru performance against libzim 9.8.2

The application search comparison uses the original libzim-backed Kiwix release build at `f2875d0` and the integration at `eccfcbe`, built with the same macOS toolchain. The integration pins MIT Zimru at `30e1710` and GPL Glass search at `7007911`. Both Kiwix builds still serve articles and media through native libzim. All measurements below used an Apple M1 Ultra and local files. The runners left the operating-system file cache in place.

## Application search

Four pairs of fresh processes searched the same 982,788,870-byte Simple Wikipedia archive. Launch order alternated by round. Each build returned the same 50 titles, paths and continuation flag for every request. Columns show medians across four processes. Each process supplied a first request and five repeat requests per case. The warm column uses the median of the four within-process medians.

| 50 results | libzim first | Integrated first | libzim warm | Integrated warm |
|---|---:|---:|---:|---:|
| Full text, “solar system”, with excerpts | 85.10 ms | 96.59 ms | 70.82 ms | 8.55 ms |
| Full text, same query, excerpts disabled | 2.95 ms | 4.03 ms | 2.93 ms | 3.71 ms |
| Title suggestions, “United” | 16.29 ms | 14.67 ms | 15.14 ms | 13.07 ms |

Only the excerpt case was the first search in each process. The same query without excerpts ran second, and suggestions ran third. The integrated build took about 13% longer for the first excerpt page and was about 8.3 times faster for repeated pages. Its median resident memory after these workloads was 52.7 MiB against 111.0 MiB in the libzim build. RSS was sampled after the workloads finished. The earlier 31-case pass covered four archives. [Alternating runs and binary fingerprints](2026-11-18-paired-search.json) · [Four-archive search comparison](2026-11-18-rust-archive-search.json)

To repeat the search comparison, use Python 3.11 or later on POSIX and two release-mode Kiwix builds with the same native SDK and frontend. Each launch uses its own temporary profile. Run from the Kiwix root with paths to the two binaries and an archive.

```sh
python3 scripts/compare-archive-search.py /path/to/libzim-kiwix /path/to/integrated-kiwix \
  /path/to/wikipedia_en_simple_all_nopic_2026-05.zim --query 'solar system' --prefix United
```

## Article and media delivery in the app

The two applications returned matching article bytes after removal of the per-session reader bridge. Image and media payloads matched native extraction byte-for-byte, including range status and length. One warm-up preceded eleven measured requests per operation and build. Requests alternated between the two running apps. Times include local HTTP and reading the response body. Browser rendering and media playback were outside the timed interval.

| Request | Original app | Integrated app |
|---|---:|---:|
| “Solar System” HTML | 0.552 ms | 0.564 ms |
| 17,826,555-byte HTML response | 6.966 ms | 7.144 ms |
| 3 MiB media response | 1.553 ms | 1.559 ms |
| 1 MiB range crossing a 1 MiB boundary | 0.827 ms | 0.836 ms |
| 4 KiB seek within media | 0.400 ms | 0.425 ms |

Across five fresh profiles per build, the median time to launch, import Simple Wikipedia, load metadata and announce readiness was 3.22 s for the original app and 2.84 s for the integrated app. The archive-open timings for the libraries themselves are reported separately below. [Content route results](2026-11-18-native-content-routes.json)

## Direct archive reading

Two standalone release drivers called the public archive APIs on the same five files. Each reader ran ten times per archive, operation and cache policy, alternating which reader ran first. Each trial used a fresh process and a warm operating-system file cache. Both drivers returned the same bytes for every sampled operation. The native default-policy run disabled eager Xapian loading, matching Kiwix. [Raw timings, archive identities and response hashes](2026-11-18-direct-archive-readers.json)

The first two columns time opening the file and reading its first selected article in the same trial. The last two time 24 distinct article reads after opening. Each read copies up to 64 KiB. Values are median milliseconds under each reader's default cache policy.

| Archive | libzim open + first | Zimru open + first | libzim 24 articles | Zimru 24 articles |
|---|---:|---:|---:|---:|
| Simple Wikipedia nopic | 6.74 | 2.27 | 23.82 | 49.10 |
| Simple Wikipedia maxi | 14.34 | 2.10 | 24.76 | 45.71 |
| Chinese Chemistry | 2.93 | 1.87 | 14.54 | 32.20 |
| Climate | 3.10 | 1.88 | 12.08 | 20.04 |
| Nollywood | 3.72 | 2.15 | 20.84 | 33.05 |

Zimru delivered the first selected article sooner on all five archives. libzim read the 24 distinct articles faster on all five, including with both cache targets set to 4 MiB. On Simple Wikipedia nopic, the median peak process memory for the 24 reads was 49.0 MiB for libzim and 60.5 MiB for Zimru with default cache targets of 16 and 64 MiB, respectively. At equal 4 MiB targets, both processes peaked near 24.5 MiB. These direct-reader measurements cover sampled prefixes and ranges. The application HTTP comparison above uses libzim for content in both builds.

The application benchmark drivers and reproduction instructions are in the Kiwix Tauri checkout at `8b2fe53d57d44d1676320818d8624103c19c9fe6`, under `scripts/bench-archive-readers/` and `scripts/compare-archive-search.py`.
