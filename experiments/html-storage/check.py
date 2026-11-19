#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Compare the app-selected HTML consumers with two isolated storage implementations."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys

HERE = Path(__file__).resolve().parent
CHECKS = HERE / "checks"
FEATURES = 'dom_query={version="=0.28.0",default-features=false}\nhtml5ever="=0.39.0"\n'
EXTRA = 'cssparser="=0.37.0"\ntokio-util="=0.7.20"\nrustix={version="1",features=["time"]}\n'


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def section(source, start, end):
    begin = source.index(start)
    return source[begin:source.index(end, begin)]


def app_consumer(app, check_limits):
    exports = (app / "src/exports.rs").read_text()
    live = (app / "src/native_pdf/live.rs").read_text()
    parse = section(exports, "pub(crate) mod html {", "\n#[cfg(test)]\nmod tests {")
    if check_limits:
        private = "pub(super) fn parse_chunked("
        assert parse.count(private) == 1
        parse = parse.replace(private, "pub(crate) fn parse_chunked(")
    bounds = [line for line in exports.splitlines()
              if line.startswith(("const MAX_REFERENCES:", "const MAX_URL:"))]
    assert len(bounds) == 2
    return "\n".join([
        "#![allow(dead_code)]", "mod cpu_time {", (app / "src/cpu_time.rs").read_text(), "}",
        "mod exports {", *bounds, section(exports, "fn inline_reference", "\n}") + "\n}",
        section(exports, "pub(crate) fn css_tokens", "/// A Wikipedia-shaped article"), parse, "}",
        "use crate::exports::html::{self,Html}; use dom_query::NodeRef; use tokio_util::sync::CancellationToken;",
        section(live, "fn escaped(", "\n#[cfg(test)]"),
        (CHECKS / ("limits.rs" if check_limits else "export.rs")).read_text(),
    ])


def build(app, work, lane, bounded, *, storage=None, extra_parser_bins=()):
    name = ("bounded-" if bounded else "parser-") + lane
    root = work / name
    bins = root / "src/bin"
    bins.mkdir(parents=True)
    if bounded:
        for bin_name in ["export", "limits"]:
            (bins / (bin_name + ".rs")).write_text(app_consumer(app, bin_name == "limits"))
    else:
        for bin_name in ("bytes", "chunk_bench", "differential", "fragment", "operations", *extra_parser_bins):
            shutil.copyfile(CHECKS / (bin_name + ".rs"), bins / (bin_name + ".rs"))
    patches = {}
    for crate, version in [("html5ever", "0.39.0"), ("markup5ever", "0.39.0"), ("cssparser", "0.37.0")]:
        if crate != "cssparser" or bounded:
            patches[crate] = app / "vendor" / (crate + "-" + version)
    if lane == "candidate":
        patches["tendril"] = storage if storage is not None else HERE / "tendril"
    manifest = '[package]\nname=' + json.dumps(name) + '\nversion="0.0.0"\nedition="2024"\n'
    manifest += '[dependencies]\n' + FEATURES + (EXTRA if bounded else 'tendril="=0.5.1"\n')
    manifest += '[patch.crates-io]\n' + ''.join(
        f"{key}={{path={json.dumps(str(path.resolve(strict=True)))}}}\n" for key, path in patches.items())
    (root / "Cargo.toml").write_text(manifest)
    # Keep the app's already resolved registry versions while pruning other workspace crates.
    shutil.copyfile(app / "Cargo.lock", root / "Cargo.lock")
    command = ["cargo", "build", "--release", "--offline", "--manifest-path", root / "Cargo.toml",
               "--target-dir", root / "target"]
    subprocess.run(command, check=True)
    return root / "target/release"


def timed(exe, input_path, destination):
    with subprocess.Popen([exe, input_path, destination], stdout=subprocess.PIPE) as child:
        _, status, usage = os.wait4(child.pid, 0)
        child.returncode = os.waitstatus_to_exitcode(status)
        if child.returncode:
            raise RuntimeError(f"{exe.name} returned {child.returncode}")
        duration_ns = int(child.stdout.read().split()[-1])
    rss = usage.ru_maxrss if sys.platform == "darwin" else usage.ru_maxrss * 1024
    return {"ns": duration_ns, "peak_rss_bytes": rss}


def measure(exes, inputs, work):
    reports = {}
    for name, source in inputs:
        output_hash = None
        samples = []
        bounded = source.is_dir()
        for pair in range(9):
            row = {}
            for lane in (["control", "candidate"] if pair % 2 == 0 else ["candidate", "control"]):
                out = work / (lane + "-export" if bounded else lane + ".html")
                if bounded:
                    out.mkdir(exist_ok=True)
                exe = exes[(lane, bounded)] / ("export" if bounded else "chunk_bench")
                row[lane] = timed(exe, source, out)
                files = sorted(out.glob("*.html")) if bounded else [out]
                assert len(files) == (4 if bounded else 1)
                hashes = [digest(f) for f in files]
                if output_hash is None:
                    output_hash = hashes
                assert hashes == output_hash
            samples.append(row)
        measured = samples[1:]
        ratios = [r["candidate"]["ns"] / r["control"]["ns"] for r in measured]
        reports[name] = {"samples": samples, "median_ns": {
            lane: statistics.median(r[lane]["ns"] for r in measured) for lane in ["control", "candidate"]
        }, "paired_median_ratio": statistics.median(ratios), "faster": sum(r < 1 for r in ratios),
            "output_sha256": output_hash}
    return reports


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repository", type=Path, help="checkout containing the checked tokenizer")
    parser.add_argument("output", type=Path, help="new, previously absent working directory")
    parser.add_argument("first_article", type=Path, help="unmodified UTF-8 article HTML")
    parser.add_argument("second_article", type=Path, help="another unmodified UTF-8 article")
    parser.add_argument("captures", type=Path, help="directory containing exactly four captured HTML files")
    parser.add_argument("--measure", action="store_true", help="run eight alternating measured pairs per case")
    args = parser.parse_args()
    app = args.repository.resolve(strict=True) / "src-tauri"
    articles = [args.first_article.resolve(strict=True), args.second_article.resolve(strict=True)]
    capture = args.captures.resolve(strict=True)
    captures = sorted(capture.iterdir())
    if len(captures) != 4 or not all(p.is_file() and p.suffix == ".html" for p in captures):
        parser.error("capture directory must contain exactly four .html files")
    work = args.output.resolve()
    work.mkdir()
    exes = {(lane, bounded): build(app, work, lane, bounded)
            for lane in ["control", "candidate"] for bounded in [False, True]}
    equal = {}
    for check in ["operations", "bytes", "differential", "fragment", "limits", "export"]:
        hashed = []
        for lane in ["control", "candidate"]:
            extra = articles if check in ["differential", "fragment"] else captures if check == "limits" else []
            executable = exes[(lane, check in ["limits", "export"])] / check
            if check == "export":
                out = work / (lane + "-export")
                out.mkdir()
                subprocess.run([executable, capture, out], check=True)
                files = sorted(out.glob("*.html"))
                assert len(files) == 4
                hashed.append([digest(f) for f in files])
            else:
                with (work / (lane + "-" + check + ".txt")).open("wb") as output:
                    subprocess.run([executable, *extra], stdout=output, check=True)
                hashed.append(digest(Path(output.name)))
        assert hashed[0] == hashed[1], check
        equal[check] = hashed[0]
    result = {"equal_output_sha256": equal, "input_sha256": [digest(p) for p in articles + captures]}
    if args.measure:
        upstream = app / "vendor/html5ever-0.39.0/data/bench"
        sources = articles + [upstream / "lipsum.html", upstream / "lipsum-zh.html"]
        cases = [(str(i) + "-" + p.name, p) for i, p in enumerate(sources)] + [("bounded-sanitizer-four", capture)]
        result["timings"] = measure(exes, cases, work)
    (work / "result.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    if not __debug__:
        sys.exit("Run with Python assertions enabled.")
    main()
