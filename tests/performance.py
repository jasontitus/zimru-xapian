"""Measure release CLI wall time and peak RSS on a fixed, warm-cache workload.

Run prepare once. Measure each pair of release binaries against the same files.
Each workload has one untimed warm-up per binary. Paired runs alternate order.
The wall clock includes launching GNU time and the CLI. Peak RSS is measured
by GNU time for the CLI process alone, in KiB. The fixture contains 50,000 small pages by default. The word "commonterm" appears
in each and "rareterm" in every thousandth. Two real archives use the files
fetched by real_archives.py. Measurements require GNU time (Linux or gtime).
"""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
from statistics import median
import subprocess
import tempfile
import time


def prepare(args):
    work = args.work
    work.mkdir(parents=True, exist_ok=True)
    html = work / "html"
    html.mkdir()
    (html / "icon.png").write_bytes(base64.b64decode(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jrL8AAAAASUVORK5CYII="
    ))
    with (work / "corpus.jsonl").open("x", encoding="utf-8") as records:
        for i in range(args.docs):
            path = f"{i:07}.html"
            title = f"Gazette {i:07}"
            terms = "commonterm " * (1 + i % 5) + f"region{i % 128} section{i % 64}"
            if i % 1000 == 0:
                terms += " rareterm"
            body = f"<html><head><title>{title}</title></head><body>{terms}</body></html>"
            (html / path).write_text(body, encoding="utf-8")
            records.write(json.dumps(dict(path=path, title=title, body=body, mimetype="text/html")) + "\n")
    subprocess.run([
        args.zimwriterfs, "--welcome=0000000.html", "--illustration=icon.png", "--language=eng",
        "--name=performance", "--title=Gazette", "--description=benchmark",
        "--creator=benchmark", "--publisher=benchmark", "--xapianbuilder-path",
        args.builder.resolve(), html, work / "synthetic.zim"
    ], check=True)


def benchmark(args):
    corpus = args.work / "corpus.jsonl"
    archives = {
        "synthetic": args.work / "synthetic.zim",
        "climate": args.real / "wikipedia_en_climate_change_mini_2024-06.zim",
        "nollywood": args.real / "wikipedia_en_nollywood_maxi_2026-10.zim",
    }
    cases = [
        ("synthetic-common", "synthetic", [], "commonterm"),
        ("synthetic-rare", "synthetic", [], "rareterm"),
        ("synthetic-and", "synthetic", [], "commonterm rareterm"),
        ("synthetic-title", "synthetic", ["--suggest"], "gazette"),
        ("synthetic-prefix", "synthetic", ["--suggest"], "000"),
        ("climate-search", "climate", [], "climate change"),
        ("climate-title", "climate", ["--suggest"], "climate"),
        ("nollywood-search", "nollywood", [], "film"),
        ("nollywood-title", "nollywood", ["--suggest"], "film"),
    ]
    with tempfile.TemporaryDirectory(dir=args.work) as directory:
        tmp = Path(directory)
        def measure(name, command, baseline=None, output=None):
            def once(binary):
                start = time.perf_counter()
                try:
                    stdout = subprocess.check_output([
                        args.time, "-f", "%M", "-o", tmp / "rss", "--", binary, *command[1:]
                    ])
                    wall = (time.perf_counter() - start) * 1000
                    peak = int((tmp / "rss").read_text())
                    if output is None:
                        hits = len(stdout.splitlines())
                        if hits == 0:
                            raise ValueError(f"{name}: expected matching hits")
                        evidence = {"hits": hits, "result_sha256": hashlib.sha256(stdout).hexdigest()}
                    else:
                        evidence = {"output_bytes": output.stat().st_size}
                    return wall, peak, evidence
                finally:
                    if output is not None:
                        output.unlink(missing_ok=True)

            binaries = {"candidate": command[0]}
            if baseline:
                binaries["baseline"] = baseline
            labels = list(binaries)
            for binary in binaries.values():
                once(binary)
            samples = {label: [] for label in labels}
            for i in range(args.runs):
                for label in (labels if i % 2 == 0 else reversed(labels)):
                    samples[label].append(once(binaries[label]))
            for label, results in samples.items():
                evidence = results[0][2]
                if any(item[2] != evidence for item in results):
                    raise ValueError(f"{name}: results varied across runs")
                if output is None and evidence != samples["candidate"][0][2]:
                    raise ValueError(f"{name}: candidate and baseline search results differ")
                print(json.dumps(dict(
                    case=name, build=label,
                    median_ms=round(median(x[0] for x in results), 2),
                    median_peak_kib=median(x[1] for x in results),
                    samples_ms=[round(x[0], 2) for x in results], peaks_kib=[x[1] for x in results],
                    **evidence
                )), flush=True)

        for name, archive, flags, query in cases:
            measure(name, [args.search, *flags, "--limit", "10", "--", archives[archive], query],
                    args.baseline_search)
        for mode in ("fulltext", "title"):
            output = tmp / f"{mode}.glass"
            measure("write-" + mode, [
                args.builder, mode, "--input", corpus, "--output", output, "--language", "eng", "--quiet"
            ], args.baseline_builder, output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    prep = sub.add_parser("prepare")
    prep.add_argument("--zimwriterfs", required=True, type=Path)
    prep.add_argument("--docs", type=int, default=50_000)
    run = sub.add_parser("measure")
    run.add_argument("--search", required=True, type=Path)
    run.add_argument("--baseline-search", type=Path, help="interleave a second reader and check identical JSON output")
    run.add_argument("--baseline-builder", type=Path, help="interleave a second writer on the same JSONL")
    run.add_argument("--real", required=True, type=Path)
    run.add_argument("--time", default=os.environ.get("GNU_TIME", "/usr/bin/time"), type=Path)
    run.add_argument("--runs", type=int, default=5)
    for cmd in (prep, run):
        cmd.add_argument("--builder", required=True, type=Path)
        cmd.add_argument("--work", required=True, type=Path)
    args = parser.parse_args()
    if args.command == "prepare":
        if args.docs < 1:
            parser.error("--docs must be positive")
        prepare(args)
    else:
        if args.runs < 1:
            parser.error("--runs must be positive")
        benchmark(args)


if __name__ == "__main__":
    main()
