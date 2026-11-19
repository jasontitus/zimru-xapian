#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Replay the published, prior safe and inlined safe HTML storage comparison."""
import argparse
import hashlib
import itertools
import json
import os
from pathlib import Path
import statistics
import subprocess
import sys

sys.dont_write_bytecode = True
import check

HERE = Path(__file__).resolve().parent
LANES = ("published", "prior", "hints")
FUNCTIONAL = ("bytes", "operations", "differential", "fragment", "limits", "export")


def require(condition, explanation):
    if not condition:
        raise ValueError(explanation)


def stdout_digest(command):
    with subprocess.Popen(command, stdout=subprocess.PIPE) as child:
        observed = hashlib.file_digest(child.stdout, "sha256").hexdigest()
        if child.wait():
            raise subprocess.CalledProcessError(child.returncode, command)
    return observed


def measure(command):
    # Each runner prints one short line with its internal nanoseconds as the final field.
    with subprocess.Popen(command, stdout=subprocess.PIPE) as child:
        output = child.stdout.read().decode().split()
        _, status, usage = os.wait4(child.pid, 0)
        child.returncode = os.waitstatus_to_exitcode(status)
        if child.returncode:
            raise subprocess.CalledProcessError(child.returncode, command)
    return output, {
        "ns": int(output[-1]),
        "process_cpu_us": round(1e6 * (usage.ru_utime + usage.ru_stime)),
        "maxrss_bytes": usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024),
    }


def experiment(args):
    recorded = json.loads((HERE / "evidence/mac-inline.json").read_text())
    prior = args.prior.resolve(strict=True) / "experiments/html-storage/tendril"
    for label, directory in (("prior", prior), ("hints", HERE / "tendril")):
        for filename, expected in recorded["storage"][label].items():
            require(check.digest(directory / filename) == expected, f"Different {label} {filename}")
    article = [p.resolve(strict=True) for p in (args.climate, args.nollywood)]
    capture_dir = args.captures.resolve(strict=True)
    captures = sorted(capture_dir.iterdir())
    require(len(captures) == 4 and all(p.is_file() and p.suffix == ".html" for p in captures),
            "Expected exactly four HTML captures")
    app = HERE.parents[1] / "src-tauri"
    cases = {
        "Climate": article[0], "Nollywood": article[1],
        "Wikipedia100": args.wikipedia.resolve(strict=True),
        "Chinese": app / "vendor/html5ever-0.39.0/data/bench/lipsum-zh.html",
        "sanitizer": capture_dir, "compact": None, "shared": None, "sliding": None,
    }
    for name, path in cases.items():
        if path is not None:
            inputs = captures if name == "sanitizer" else [path]
            require([check.digest(p) for p in inputs] == list(recorded["inputs"][name].values()),
                    f"Input bytes differ in {name}")
    out = args.output.resolve()
    out.mkdir()  # Keep all existing files and previous trials.
    binaries = {}
    binary_hashes = {}
    for lane in LANES:
        storage = prior if lane == "prior" else HERE / "tendril"
        for bounded in (False, True):
            binaries[lane, bounded] = check.build(
                app, out / lane, "control" if lane == "published" else "candidate", bounded,
                storage=storage, extra_parser_bins=("shapes",))
        binary_hashes[lane] = {
            name: check.digest(binaries[lane, name in ("export", "limits")] / name)
            for name in (*FUNCTIONAL, "chunk_bench", "shapes")
        }
        for name in FUNCTIONAL:
            executable = binaries[lane, name in ("export", "limits")] / name
            if name == "export":
                dest = out / lane / "verified-export"
                dest.mkdir()
                subprocess.run([executable, capture_dir, dest], check=True)
                actual = [check.digest(p) for p in sorted(dest.glob("*.html"))]
            else:
                inputs = article if name in ("fragment", "differential") else captures if name == "limits" else []
                actual = stdout_digest([executable, *inputs])
            require(actual == recorded["checks"][name], f"{lane} {name} outputs differ")

    results = {}
    for name, path in cases.items():
        mode = "shapes" if path is None else "export" if name == "sanitizer" else "chunk_bench"
        rows = []
        widths = {}
        for order in [LANES, *list(itertools.permutations(LANES)) * 2]:
            row = {"order": order}
            for lane in order:
                dest = out / lane / "timed-output"
                dest.mkdir(exist_ok=True)
                files = [] if mode == "shapes" else [
                    dest / f"{i}.html" for i in range(4 if mode == "export" else 1)]
                for previous in files:
                    previous.unlink(missing_ok=True)
                argv = [name] if mode == "shapes" else [path, dest if mode == "export" else files[0]]
                output, row[lane] = measure([binaries[lane, mode == "export"] / mode, *argv])
                observed = [check.digest(p) for p in files] if files else output[1:3]
                require(observed == recorded["trials"][name]["checked_result"], f"{lane} {name} result differs")
                if mode == "shapes":
                    widths[lane] = int(output[0])
            rows.append(row)
        retained = rows[1:]
        summary = {lane: {field: statistics.median(r[lane][field] for r in retained)
                          for field in retained[0][lane]} for lane in LANES}
        for baseline in LANES[:2]:
            ratios = [r["hints"]["ns"] / r[baseline]["ns"] for r in retained]
            summary["hints/" + baseline] = {"median_paired_ratio": statistics.median(ratios),
                                            "hints_faster": sum(r < 1 for r in ratios)}
        results[name] = {"trials": rows, "summary": summary, "object_bytes": widths}
        print(name, summary["hints/prior"], summary["hints/published"], flush=True)
    (out / "results.json").write_text(json.dumps({"binaries": binary_hashes, "trials": results}, indent=2) + "\n")


if __name__ == "__main__":
    if sys.platform not in ("darwin", "linux"):
        sys.exit("This process CPU and RSS comparison supports macOS and Linux.")
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("prior", "output", "climate", "nollywood", "wikipedia", "captures"):
        parser.add_argument(name, type=Path)
    experiment(parser.parse_args())
