#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Requires python-libzim and zimwriterfs. Both writers must be built.
set -euo pipefail
if [[ $# != 4 ]]; then
    echo "usage: $0 ZIMWRITERFS NATIVE_BUILDER RUST_BUILDER ZIMSEARCH" >&2
    exit 2
fi
zimwriterfs=$1
native_builder=$2
rust_builder=$3
zimsearch=$4
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
mkdir "$work/html"
echo '<html><head><title>Welcome</title></head><body>Home</body></html>' > "$work/html/index.html"
echo '<html><head><title>Mollusca</title></head><body>Molluscs are invertebrate animals.</body></html>' > "$work/html/Mollusca.html"
# Tiny PNG for zimwriterfs's required archive illustration.
"${PYTHON:-python3}" - "$work/html/icon.png" <<'PY'
import base64, pathlib, sys
pathlib.Path(sys.argv[1]).write_bytes(base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jrL8AAAAASUVORK5CYII="))
PY
for variant in native rust; do
    builder=$native_builder
    if [[ $variant == rust ]]; then builder=$rust_builder; fi
    archive="$work/$variant.zim"
    "$zimwriterfs" --welcome=index.html --illustration=icon.png --language=eng \
        --name=demo --title=Demo --description=interop --creator=test --publisher=test \
        --xapianbuilder-path "$builder" "$work/html" "$archive"
    "${PYTHON:-python3}" - "$archive" "$zimsearch" <<'PY'
import json, subprocess, sys
from libzim.reader import Archive
from libzim.search import Query, Searcher
from libzim.suggestion import SuggestionSearcher

filename, cli = sys.argv[1:]
archive = Archive(filename)
for args, query in (([], "animals"), (["--suggest"], "Moll"), ([], "zzzzabsent")):
    results = (SuggestionSearcher(archive).suggest(query) if args else
               Searcher(archive).search(Query().set_query(query)))
    native = ["C/" + path for path in results.getResults(0, 10)]
    output = subprocess.check_output([cli, *args, filename, query], text=True)
    rust = [json.loads(line)["path"] for line in output.splitlines()]
    expected = [] if query == "zzzzabsent" else ["C/Mollusca.html"]
    if rust != expected or native != expected:
        raise SystemExit(f"{filename} {query!r}: expected {expected}, Rust {rust}, libzim {native}")
print(f"{filename}: full text, title suggestion and empty search passed with both readers")
PY
done
