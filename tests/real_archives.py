"""Download two checksummed Kiwix fixtures and run engine and CLI conformance tests.

Usage: python3 tests/real_archives.py
ZIM_TESTDATA can point to a directory already containing the archives.
The four golden files were produced by libzim 9.3.0 (Xapian 1.4.23) and
copied from gordonbrander/zim-mcp@3632a45929b907109eed592653a5dd35f43c1f75,
crates/zimfile/tests/golden/xapian. Each captures 30 queries' top 10 paths.
"""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from urllib.request import urlopen


ROOT = Path(__file__).resolve().parent.parent
DATA = Path(os.environ.get("ZIM_TESTDATA", ROOT / "target" / "real-archives")).resolve()
ARCHIVES = {
    "climate": (
        "https://raw.githubusercontent.com/openzim/zim-testing-suite/"
        "2edf72096208e60c82b6ebbe313baa552cc6af52/data/nons/"
        "wikipedia_en_climate_change_mini_2024-06.zim",
        "3485a2c475a2356331f33f137ce26ed7116e62c7062d73a7773362d6dff005f4",
    ),
    "nollywood": (
        "https://download.kiwix.org/zim/wikipedia/wikipedia_en_nollywood_maxi_2026-10.zim",
        "faa53e0c8ef641206867ce21f0e107d4e2aa9012e3b2263f05fe9bddf4f72aa3",
    ),
}


def verify(path, expected):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    if digest.hexdigest() != expected:
        raise ValueError(f"SHA-256 mismatch for {path}")


def fixture(url, digest):
    destination = DATA / url.rsplit("/", 1)[1]
    if destination.exists():
        verify(destination, digest)
        return destination
    with tempfile.TemporaryDirectory(dir=DATA) as temp:
        staged = Path(temp) / destination.name
        with urlopen(url, timeout=90) as response, staged.open("wb") as out:
            shutil.copyfileobj(response, out)
        verify(staged, digest)
        staged.replace(destination)
    return destination


def golden_cases(file):
    cases = []
    for line in file.read_text(encoding="utf-8").splitlines():
        if line.startswith("== "):
            cases.append((line[3:], []))
        elif not line.startswith("# "):
            cases[-1][1].append("C/" + line)
    if len(cases) != 30:
        raise ValueError(f"incomplete reference queries in {file}")
    return cases


def main():
    DATA.mkdir(parents=True, exist_ok=True)
    fixtures = {name: fixture(*source) for name, source in ARCHIVES.items()}
    env = dict(os.environ, ZIM_TESTDATA=str(DATA))
    subprocess.run(
        ["cargo", "test", "--locked", "--release", "-p", "zimfile-xapian", "--lib", "--", "--include-ignored"],
        cwd=ROOT, env=env, check=True,
    )
    build = subprocess.run(
        ["cargo", "build", "--locked", "--release", "--bin", "zimsearch", "--message-format=json"],
        cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True,
    )
    binary = next(
        item["executable"]
        for item in map(json.loads, build.stdout.splitlines())
        if item.get("executable") and item.get("target", {}).get("name") == "zimsearch"
    )
    for name, archive in fixtures.items():
        for mode in ("search", "suggest"):
            flags = ["--suggest"] if mode == "suggest" else []
            for query, expected in golden_cases(ROOT / "tests/golden" / f"{name}.{mode}.txt"):
                result = subprocess.run(
                    [binary, *flags, "--limit", "10", "--", archive, query],
                    check=True, stdout=subprocess.PIPE, encoding="utf-8",
                )
                paths = [json.loads(line)["path"] for line in result.stdout.splitlines()]
                if paths != expected:
                    raise ValueError(f"{name} {mode} {query!r}: expected {expected!r}, got {paths!r}")
            print(f"{name} {mode}: 30/30 ordered top-10 lists match libzim", flush=True)


if __name__ == "__main__":
    main()
