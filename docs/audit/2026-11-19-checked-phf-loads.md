# Checked input loads for perfect hash lookups

This branch examines the input path from archive HTML names and CSS tokens through `phf_shared`. The app resolves one SipHasher 1.0.4 package through Cargo. Generated tables use SipHash-1-3 with a 128-bit result, both when building the tables and at lookup. Their keys and hash values must agree across hosts and targets.

The local dual-licensed library keeps that algorithm and replaces the input loads. Full words are read through a checked slice and decoded in little-endian order. The input tail is checked, copied into an initialized eight-byte array and decoded with the same byte order. The crate denies unsafe code except for the existing volatile read in `Hash128::eq`. That reads an initialized local integer. The app's PHF function destructures the hash result into integers. The equality operation preserves upstream behavior and is outside that lookup path.

The differential runner compared 34,944 one-shot results and 594,048 partitioned results against the exact published crate, including both SipHash round variants and both widths. It varied byte alignment, empty input, length, tail width, write boundaries and keys. The tests compare both words of the 128-bit result. The upstream tests passed with default, no default and all features. In a separate checked-atom DOM harness, namespace and selector output and four full document serializations matched with both hash implementations. The documents included two Wikipedia articles and two generated inputs with 20,000 custom-name elements.

```sh
cargo run --locked --release --manifest-path scripts/check-siphash-loads/Cargo.toml
cargo test --locked --release --manifest-path src-tauri/vendor/siphasher-1.0.4/Cargo.toml
cargo test --locked --no-default-features --manifest-path src-tauri/vendor/siphasher-1.0.4/Cargo.toml
```

The evidence covers hash equivalence and the isolated DOM. Complete app runtime acceptance and cross-platform performance measurements are open. The application branch uses the published SipHasher while this candidate is under evaluation.
