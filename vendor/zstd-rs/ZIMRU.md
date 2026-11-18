# Zstandard codec provenance

This directory carries the source and tests of `zstd-rs` 0.1.0 from crates.io. The release records upstream commit `bac4c37d86e5307537c145823a106f7ed8f386d7`, subdirectory `zstd-rs`. The published crate archive has SHA-256 `41514ccc30389f95bb9e6ab6d5634f17121c7b4fead6722a5fceec1c7891d78f`. [Upstream repository](https://github.com/Avarok-Cybersecurity/zstd-rs)

The library is licensed under MIT or Apache-2.0. Both license texts accompany the source. The Zstandard reference golden fixtures in `tests/golden` carry the upstream BSD-3-Clause notice in that directory.

The local manifest builds the library without a build script or runtime dependencies. The upstream examples and benchmarks are omitted. The native development dependency provides a separate format oracle when testing this directory directly.

The local decoder adds a block-at-a-time frame API in `src/decode/mod.rs` and exports it through `src/lib.rs`. It checks the declared window, frame pledge, block sizes, total output and optional checksum. The output doubles as the frame history. Allocation grows incrementally across frames within the caller's total output bound. Sequence execution in `src/decode/execute.rs` enforces the frame's history window, including the RFC 8878 dictionary exception, and handles matches ending at the exact output cap. Short matches use checked 8- or 16-byte array copies when the output buffer has spare space. A match ending near the allocation limit uses exact-length copies. The encoder's fast-level range in `src/encode/params.rs` includes the reference lower bound of −131072.

Run the upstream interoperability tests from the ZIM repository root.

```sh
cargo test --locked --release --manifest-path vendor/zstd-rs/Cargo.toml
```

These commands compile the fork with unsafe code forbidden and list its production dependencies.

```sh
RUSTFLAGS=-Funsafe-code cargo build --locked --release --lib --manifest-path vendor/zstd-rs/Cargo.toml
cargo tree --locked --edges normal --manifest-path vendor/zstd-rs/Cargo.toml
```
