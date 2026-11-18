"""Compare three MIT builds, checking each archive or frame with native tools.

Works on macOS/Linux. See docs/writer-benchmark-20261118.md for setup.
"""
import argparse
import csv
import hashlib
import json
import os
import re
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import time
import zlib


def measured(command):
    """Measure only the child under test (including its worker threads)."""
    start = time.perf_counter()
    # Popen/wait4 expose this child's CPU and high-water mark without counting
    # the native tools which verify the result in separate processes.
    with tempfile.TemporaryFile() as log:
        proc = subprocess.Popen(command, stdout=log, stderr=log)
        _, status, ru = os.wait4(proc.pid, 0)
        elapsed = time.perf_counter() - start
        proc.returncode = os.waitstatus_to_exitcode(status)
        if proc.returncode:
            log.seek(0)
            raise RuntimeError(f"{command!r}: {log.read().decode(errors='replace')}")
    rss = ru.ru_maxrss if sys.platform == "darwin" else ru.ru_maxrss * 1024
    return elapsed, ru.ru_utime, ru.ru_stime, rss


def content(reader, path):
    raw = subprocess.check_output([reader, "readall", path, "--content-md5", "--quiet"], text=True)
    fields = dict(line.split(":", 1) for line in raw.splitlines())
    count, digest = int(fields["content_entries"]), fields["content_md5"].strip()
    if count <= 0 or not re.fullmatch(r"[0-9a-f]{32}", digest):
        raise ValueError(f"Incomplete archive content report for {path}")
    return count, digest


def digest_stream(stream):
    digest = hashlib.sha256()
    length = 0
    while block := stream.read(1024 * 1024):
        digest.update(block)
        length += len(block)
    return length, digest.hexdigest()


def pinned_input(name, path):
    fixture_file = Path(__file__).resolve().parents[1] / "docs/review-data/writer-macos-20261118/inputs.json"
    fixture = json.loads(fixture_file.read_text())[name]
    with path.open("rb") as stream:
        if digest_stream(stream)[1] != fixture["sha256"]:
            raise ValueError(f"The {name} archive differs from the pinned fixture")
    return fixture


def decode_hash(command):
    with subprocess.Popen(command, stdout=subprocess.PIPE) as proc:
        result = digest_stream(proc.stdout)
        if proc.wait():
            raise RuntimeError(f"Native decode failed: {command!r}")
        return result


def check_zim(path):
    subprocess.run(["zimcheck", "-I", "-C", path], check=True, stdout=subprocess.DEVNULL)


def whole(args, scratch):
    builds = {"native": args.native, "stable": args.stable, "tree": args.tree}
    inputs = dict(zip(("chemistry", "climate", "nollywood"), args.archives))
    oracle = args.native / "zimru"
    reference = {}
    for name, path in inputs.items():
        fixture = pinned_input(name, path)
        reference[name] = content(oracle, path)
        if reference[name] != (fixture["entries"], fixture["digest"]):
            raise ValueError(f"The reader disagrees with the pinned {name} content")
    rows = csv.writer(sys.stdout)
    rows.writerow("archive level workers round build elapsed_s user_s system_s peak_rss_bytes output_bytes checked_content_entries digest zimcheck".split())
    target = scratch / "recreated.zim"
    for rnd in range(1, args.rounds + 1):
        for level in (3, 19):
            for idx, (name, path) in enumerate(inputs.items()):
                for workers in (1, 4):
                    order = ["tree", "native", "stable"]
                    pos = (rnd + idx + (workers == 4)) % 3
                    for build in order[pos:] + order[:pos]:
                        timing = measured([builds[build] / "zimrecreate", path, target,
                                           "--without-indexes", "--compression", "zstd",
                                           "--compression-level", str(level), "--threads", str(workers)])
                        verified = content(oracle, target)
                        if reference[name] != verified:
                            raise ValueError(f"Archive content differs: {name}, {build}")
                        check_zim(target)
                        rows.writerow([name, level, workers, rnd, build, *timing,
                                       target.stat().st_size, *verified, "pass"])
                        sys.stdout.flush()
                        target.unlink()


def climate_clusters(path, count):
    """Read only the trusted, pinned Climate test fixture's first Zstd clusters."""
    data = path.read_bytes()
    num = struct.unpack_from("<I", data, 28)[0]
    ptr = struct.unpack_from("<Q", data, 48)[0]
    starts = struct.unpack_from(f"<{num}Q", data, ptr)
    raw = bytearray()
    for left, right in zip(starts, starts[1:]):
        if data[left] & 15 == 5:
            block = subprocess.check_output(["zstd", "-dqc"], input=data[left + 1:right])
            raw.extend(block)
            if len(raw) >= count:
                return raw[:count]
    raise ValueError("Expected 12 MiB of content from the pinned Climate fixture")


def history(args, scratch):
    pinned_input("climate", args.climate)
    raw = climate_clusters(args.climate, 12 << 20)
    rows = csv.writer(sys.stdout)
    rows.writerow("pattern level workers build elapsed_s user_s system_s peak_rss_bytes raw_bytes output_bytes sha256_native_decoded".split())
    target = scratch / "encoded.zst"
    path = scratch / "input"
    for label, first, copies in [("adjacent", 12 << 20, 1), ("repeat", 3 << 20, 4)]:
        path.write_bytes(raw[:first])
        expected = len(raw), hashlib.sha256(raw[:first] * copies).hexdigest()
        for level in (3, 19):
            for workers in (1, 4):
                for build, exe, mode in [("native", args.tree, "native"), ("stable", args.stable, "rust"), ("tree", args.tree, "rust")]:
                    timing = measured([exe, mode, str(level), str(workers), path, str(copies), target])
                    actual = decode_hash(["zstd", "-dqc", target])
                    if actual != expected:
                        raise ValueError(f"Decoded frame differs: {label}, {build}")
                    rows.writerow([label, level, workers, build, *timing, actual[0], target.stat().st_size, actual[1]])
                    sys.stdout.flush()
                    target.unlink()


def large(args, scratch):
    src = scratch / "input"
    src.mkdir()
    (src / "index.html").write_text("<title>Large item streaming check</title><p>Data</p>")
    def chunk(name, body):
        return struct.pack(">I", len(body)) + name + body + struct.pack(">I", zlib.crc32(name + body))
    png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 48, 48, 8, 2, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress((b"\x00" + b"\x50\x70\x80" * 48) * 48)) + chunk(b"IEND", b""))
    (src / "icon.png").write_bytes(png)
    path = src / "big.txt"
    length = (1 << 30) + (1 << 17) + 23
    with path.open("wb") as f:
        for offset in range(0, length, 2 << 20):
            f.seek(offset)
            f.write(f"boundary {offset}\n".encode())
        f.seek(length - 4)
        f.write(b"end\n")
    with path.open("rb") as f:
        expected = digest_stream(f)
    out = scratch / "big.zim"
    timing = measured([args.tree, "--without-indexes", "--welcome=index.html", "--illustration=icon.png",
                       "--language=eng", "--name=check", "--title=Check", "--description=Check",
                       "--creator=Test", "--publisher=Test", "--compression-level=19", "--threads=4", src, out])
    check_zim(out)
    actual = decode_hash(["zimdump", "show", "--url=big.txt", out])
    if actual != expected:
        raise ValueError("Native extraction differs")
    rows = csv.writer(sys.stdout)
    rows.writerow("level workers elapsed_s user_s system_s peak_rss_bytes raw_bytes output_bytes sha256".split())
    rows.writerow([19, 4, *timing, actual[0], out.stat().st_size, actual[1]])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subs = parser.add_subparsers(dest="mode", required=True)
    w = subs.add_parser("whole", help="Directories containing zimrecreate and zimru, then the three ZIMs")
    for name in ("native", "stable", "tree"):
        w.add_argument(name, type=Path)
    w.add_argument("archives", type=Path, nargs=3, metavar="ARCHIVE")
    w.add_argument("--rounds", type=int, default=3)
    h = subs.add_parser("history", help="Stable and tree writer-codec-check binaries, then the Climate ZIM")
    for name in ("stable", "tree", "climate"):
        h.add_argument(name, type=Path)
    subs.add_parser("large", help="Tree zimwriterfs binary").add_argument("tree", type=Path)
    args = parser.parse_args()
    if sys.platform not in ("darwin", "linux"):
        parser.error("Peak RSS reporting is supported on macOS and Linux")
    if args.mode == "whole" and args.rounds < 1:
        parser.error("--rounds must be positive")
    with tempfile.TemporaryDirectory() as scratch:
        {"whole": whole, "history": history, "large": large}[args.mode](args, Path(scratch))


if __name__ == "__main__":
    main()
