"""Compare two release builds on the same ZIM. See docs/rust-codecs.md."""
import argparse
import csv
import hashlib
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def digest(reader, archive):
    return run(reader, "readall", archive, "--content-md5", "--quiet")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reference", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--codec", choices=["zstd", "xz"], default="zstd")
    parser.add_argument("--levels", type=int, nargs="*", default=[3, 19])
    parser.add_argument("--workers", type=int, default=1)
    args = parser.parse_args()
    builds = [("native", args.reference.resolve()), ("rust", args.candidate.resolve())]
    oracle = builds[0][1] / "zimru"
    expected = digest(oracle, args.archive)
    with args.archive.open("rb") as src:
        archive_hash = hashlib.file_digest(src, "sha256").hexdigest()
    w = csv.writer(sys.stdout)
    w.writerow(["archive", "sha256", "operation", "level", "workers", "round",
                "build", "elapsed_ns", "bytes"])
    verified_size = None
    with tempfile.TemporaryDirectory() as temp:
        output = Path(temp) / "recreated.zim"
        # Round zero warms both paths. Each timed round reverses build order.
        for i in range(args.rounds + 1):
            for name, build in builds[::1 if i % 2 == 0 else -1]:
                ns, decoded = map(int, run(build / "codec_read", args.archive).split())
                if verified_size is None:
                    verified_size = decoded
                if decoded != verified_size:
                    raise ValueError("decoded blob byte counts differ")
                if i:
                    w.writerow([args.archive.name, archive_hash, "decode", "", 1, i,
                                name, ns, decoded])
                for level in args.levels:
                    start = time.perf_counter_ns()
                    run(build / "zimrecreate", args.archive, output, "--without-indexes",
                        "--compression", args.codec, "--compression-level", str(level),
                        "--threads", str(args.workers))
                    elapsed = time.perf_counter_ns() - start
                    if digest(oracle, output) != expected:
                        raise ValueError(f"{name} changed archive content at level {level}")
                    if i:
                        w.writerow([args.archive.name, archive_hash, args.codec, level,
                                    args.workers, i, name, elapsed, output.stat().st_size])
                    output.unlink()
                sys.stdout.flush()


if __name__ == "__main__":
    main()
