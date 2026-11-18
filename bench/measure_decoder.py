"""Compare the uncached-cluster example from three MIT builds on macOS or Linux."""
import argparse
import hashlib
import itertools
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile

def sha256(stream):
    result = hashlib.sha256()
    while chunk := stream.read(1 << 20):
        result.update(chunk)
    return result.hexdigest()

def archive_identity(path):
    with path.open("rb") as file:
        return sha256(file)

def digest(binary, path):
    with subprocess.Popen([binary, path], stdout=subprocess.PIPE) as proc:
        result = sha256(proc.stdout)
        if proc.wait():
            raise RuntimeError(f"Failed to read complete blob manifest using {binary}")
    return result

def scan(binary, path):
    with tempfile.TemporaryFile() as out:
        proc = subprocess.Popen([binary, path], stdout=out)
        _, status, usage = os.wait4(proc.pid, 0)
        proc.returncode = os.waitstatus_to_exitcode(status)
        if proc.returncode:
            raise RuntimeError(f"Scan failed: {binary}, {path}")
        out.seek(0)
        nanos, decoded = map(int, out.read().split())
    if nanos <= 0 or decoded <= 0:
        raise ValueError(f"Empty benchmark sample for {path}")
    return {
        "elapsed_ms": nanos / 1e6,
        "cpu_ms": (usage.ru_utime + usage.ru_stime) * 1000,
        "peak_rss_bytes": usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024),
        "blob_bytes": decoded,
    }

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("native", "production", "experiment"):
        parser.add_argument(name, type=Path, help="Directory holding codec_read and blob_manifest")
    parser.add_argument("archives", nargs="+", type=Path)
    parser.add_argument("--rounds", default=25, type=int)
    args = parser.parse_args()
    if sys.platform not in ("linux", "darwin") or args.rounds < 1:
        parser.error("Requires macOS or Linux and a positive round count")
    builds = {k: getattr(args, k).resolve() for k in ("native", "production", "experiment")}
    hashes = {}
    for name in ("codec_read", "blob_manifest"):
        binaries = {k: archive_identity(v / name) for k, v in builds.items()}
        if len(set(binaries.values())) != len(builds):
            raise ValueError(f"The {name} binaries must be distinct. Use separate Cargo targets.")
        hashes[name] = binaries
    report = {"host": platform.platform(), "rounds": args.rounds, "binary_sha256": hashes,
              "archives": []}
    for path in args.archives:
        manifests = {k: digest(base / "blob_manifest", path) for k, base in builds.items()}
        if len(set(manifests.values())) != 1 or next(iter(manifests.values())) == hashlib.sha256(b"").hexdigest():
            raise ValueError(f"Decoded archive contents differ or are empty: {path}")
        samples = {k: [] for k in builds}
        orders = itertools.cycle(itertools.permutations(builds))
        for iteration in range(args.rounds + 1):
            for k in next(orders):
                row = scan(builds[k] / "codec_read", path)
                if iteration:
                    samples[k].append(row)
        if len({r["blob_bytes"] for subset in samples.values() for r in subset}) != 1:
            raise ValueError(f"Decoded byte counts differ: {path}")
        report["archives"].append({"file": path.name, "sha256": archive_identity(path),
                                   "manifest_sha256": next(iter(manifests.values())),
                                   "samples": samples})
    json.dump(report, sys.stdout, indent=2)
    print()

if __name__ == "__main__":
    main()
