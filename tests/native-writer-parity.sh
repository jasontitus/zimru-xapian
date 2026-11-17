#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Usage: tests/native-writer-parity.sh NATIVE_BUILDER_CHECKOUT NATIVE_BINARY RUST_BINARY
# Requires xapian-check and a C++17 toolchain with libzim and xapian-core
# pkg-config metadata. SNAPSHOT_HELPER may name an existing upstream-helper.
set -euo pipefail
if [[ $# != 3 ]]; then
    echo "usage: $0 NATIVE_BUILDER_CHECKOUT NATIVE_BINARY RUST_BINARY" >&2
    exit 2
fi
native_checkout=$1
native=$2
candidate=$3
fixture=$native_checkout/bench/fixture.jsonl
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT

if [[ -n ${SNAPSHOT_HELPER:-} ]]; then
    oracle=$SNAPSHOT_HELPER
else
    oracle=$work/upstream-helper
    # pkg-config emits separate compiler/linker arguments.
    read -r -a flags <<< "$(pkg-config --cflags --libs libzim xapian-core)"
    "${CXX:-c++}" -std=c++17 "$native_checkout/bench/upstream-helper.cc" -o "$oracle" "${flags[@]}"
fi

# "en" loads the exact stopword list, "eng" leaves it empty, and "fr-FR"
# selects a locale-derived stemmer while preserving the recorded language.
for lang in eng en fr-FR; do
    for mode in title fulltext; do
        name="$lang-$mode"
        for implementation in native candidate; do
            bin=$candidate
            if [[ $implementation == native ]]; then bin=$native; fi
            db="$work/$implementation-$name"
            "$bin" "$mode" --input "$fixture" --output "$db" --language "$lang" --jobs 1 --quiet
            xapian-check "$db" > /dev/null
            "$oracle" snapshot "$db" > "$db.snapshot"
        done
        diff -u "$work/native-$name.snapshot" "$work/candidate-$name.snapshot"
        echo "Matched $lang $mode"
    done
done
