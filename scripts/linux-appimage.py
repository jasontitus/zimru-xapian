#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Build and inspect the Ubuntu x86-64 system-runtime AppImage without executing it."""
import argparse
import configparser
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
TAURI = ROOT / "src-tauri"
UPSTREAM = ROOT / "upstream/appimage"
RUNTIME = UPSTREAM / "runtime-x86_64"
NOTICES = ("LICENSE", "SOURCE.txt", "type2-runtime-fedfad51.tar.gz", "fuse-3.15.0.tar.xz",
           "squashfuse-0.5.2.tar.gz", "third-party/libfuse-LGPL-2", "third-party/squashfuse-LICENSE",
           "third-party/mimalloc-MIT", "third-party/musl-COPYRIGHT", "third-party/zlib-LICENSE",
           "third-party/zstd-BSD")
RUNTIME_SHA = "617aa77314d57c8b8e4fae9c3f0b35af7b30d5529cb69e98f64f92510ed98a46"
ID = "org.kiwix.workbench"
APP = "kiwix-workbench"
BIN = Path("usr/bin")
RES = Path("usr/lib") / ID
ICON = "usr/share/icons/hicolor/{}/apps/kiwix-workbench.png"
DESKTOP = Path("usr/share/applications") / f"{ID}.desktop"
MARKER = b"__TAURI_BUNDLE_TYPE_VAR_"
LINKS = {f"{ID}.desktop": str(DESKTOP), f"{ID}.png": ICON.format("256x256@2"),
         f"{APP}.png": f"{ID}.png", ".DirIcon": f"{ID}.png"}


def require(ok, message):
    if not ok:
        raise ValueError(message)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").digest()


def reference_files():
    files = {Path("AppRun"): TAURI / "linux/AppRun"}
    for name in NOTICES:
        files[RES / "licenses/appimage-runtime" / name] = UPSTREAM / name
    config = json.loads((TAURI / "tauri.conf.json").read_text())
    for rel, destination in config["bundle"]["resources"].items():
        source = TAURI / rel
        if source.is_dir():
            for item in source.rglob("*"):
                if item.is_file():
                    files[RES / destination / item.relative_to(source)] = item
        else:
            files[RES / destination] = source
    for size, source in (("32x32", "32x32.png"), ("128x128", "128x128.png"),
                         ("256x256@2", "128x128@2x.png")):
        files[Path(ICON.format(size))] = TAURI / "icons" / source
    return files


def check_tree(root, binary=None, relay=None):
    references = reference_files()
    regular = {BIN / APP, BIN / "kiwix-mcp", DESKTOP, *references}
    directories = {parent for rel in regular for parent in rel.parents}
    for path in root.rglob("*"):
        rel = path.relative_to(root)
        mode = path.lstat().st_mode
        if str(rel) in LINKS:
            require(stat.S_ISLNK(mode) and os.readlink(path) == LINKS[str(rel)],
                    f"Invalid package link {rel}")
        elif stat.S_ISDIR(mode):
            require(rel in directories and mode & 0o555 == 0o555, f"Unexpected or inaccessible package directory {rel}")
        else:
            require(stat.S_ISREG(mode) and rel in regular and not mode & 0o6000 and mode & 0o444 == 0o444,
                    f"Unexpected package file {rel}")
            with path.open("rb") as f:
                header = f.read(20)
            native = rel in (BIN / APP, BIN / "kiwix-mcp")
            require(header.startswith(b"\x7fELF") == native, f"Unexpected executable format in {rel}")
            if native:
                require(header[:6] == b"\x7fELF\x02\x01" and header[18:20] == b"\x3e\x00",
                        f"Expected x86-64 ELF in {rel}")
            if native or rel == Path("AppRun"):
                require(mode & 0o111 == 0o111, f"Missing execute permissions on {rel}")
            regular.remove(rel)
    require(not regular and all((root / link).is_symlink() for link in LINKS),
            f"Incomplete package tree; missing {sorted(map(str, regular))}")
    for rel, source in references.items():
        require(digest(root / rel) == digest(source), f"Source content differs in {rel}")
    entry = configparser.ConfigParser(interpolation=None)
    entry.optionxform = str
    entry.read(root / DESKTOP)
    d = entry["Desktop Entry"]
    require(entry.sections() == ["Desktop Entry"] and
            set(d) == {"Categories", "Comment", "Exec", "StartupWMClass", "Icon", "Name", "Terminal", "Type", "MimeType"} and
            d.get("Categories") == "Education;" and d.get("Comment") == json.loads((TAURI / "tauri.conf.json").read_text())["bundle"]["shortDescription"] and d.get("Type") == "Application" and
            d.get("Terminal") == "false" and not d.get("Actions") and d.get("Exec") == APP + " %U" and d.get("Icon") == APP and
            d.get("StartupWMClass") == ID and d.get("Name") == "Kiwix" and
            {"application/x-zim", "x-scheme-handler/kiwix", ""} == set(d.get("MimeType", "").split(";")),
            "Desktop entry lost its executable, icon, name, GeoClue identity or file associations")
    app = (root / BIN / APP).read_bytes()
    require(app.count(MARKER + b"APP") == 1 and app.count(MARKER) == 1,
            "Packaged app lacks its AppImage updater type")
    if binary:
        plain = binary.read_bytes()
        require(plain.count(MARKER + b"UNK") == 1 and
                app == plain.replace(MARKER + b"UNK", MARKER + b"APP", 1),
                "Packaged application differs from the native build")
        require(digest(root / BIN / "kiwix-mcp") == digest(relay), "Packaged relay differs from build")


def runtime():
    data = RUNTIME.read_bytes()
    require(hashlib.sha256(data).hexdigest() == RUNTIME_SHA, "Pinned AppImage runtime checksum mismatch")
    return data


def inspect(image, binary=None, relay=None):
    prefix = runtime()
    with image.open("rb") as stream:
        require(stream.read(len(prefix)) == prefix, "AppImage loader is not the pinned runtime")
        header = stream.read(96)
        require(len(header) == 96 and header[:4] == b"hsqs" and
                struct.unpack_from("<Q", header, 40)[0] == image.stat().st_size - len(prefix),
                "Expected one complete unpadded SquashFS payload")
    with tempfile.TemporaryDirectory(prefix="kiwix-image-check-") as tmp:
        tree = Path(tmp) / "root"
        subprocess.run(["unsquashfs", "-quiet", "-strict-errors", "-no-progress", "-no-xattrs",
                        "-offset", str(len(prefix)), "-dest", tree, image], check=True)
        check_tree(tree, binary, relay)


def read_deb(deb, tree, binary, relay):
    """Copy only the canonical regular payload into a newly created AppDir."""
    limits = {rel: src.stat().st_size for rel, src in reference_files().items()
              if rel != Path("AppRun") and (RES / "licenses/appimage-runtime") not in rel.parents}
    limits.update({BIN / APP: binary.stat().st_size, BIN / "kiwix-mcp": relay.stat().st_size,
                   DESKTOP: 64 * 1024})
    directories = {parent for rel in limits for parent in rel.parents}
    tree.mkdir()
    with subprocess.Popen(["dpkg-deb", "--fsys-tarfile", deb], stdout=subprocess.PIPE) as process:
        with process.stdout, tarfile.open(fileobj=process.stdout, mode="r|") as entries:
            for entry in entries:
                rel = PurePosixPath(entry.name)
                require(not rel.is_absolute() and ".." not in rel.parts, "Invalid Debian payload path")
                if entry.isdir():
                    require(rel in directories, f"Unexpected Debian directory {rel}")
                    continue
                require(entry.isreg() and rel in limits, f"Unexpected Debian payload entry {rel}")
                size = limits.pop(rel)
                require(entry.size <= size if rel == DESKTOP else entry.size == size,
                        f"Debian payload size differs in {rel}")
                target = tree / rel
                target.parent.mkdir(parents=True, exist_ok=True)
                with target.open("xb") as output, entries.extractfile(entry) as source:
                    shutil.copyfileobj(source, output)
                target.chmod(0o755 if rel in (BIN / APP, BIN / "kiwix-mcp") else 0o644)
        process.stdout.close()
        require(process.wait() == 0, "Reading the Debian payload failed")
    require(not limits, f"Missing Debian payload files: {sorted(map(str, limits))}")


def assemble(deb, binary, relay, out):
    require(platform.system() == "Linux" and platform.machine() == "x86_64",
            "Build the x86-64 system-runtime AppImage on Ubuntu 24.04 x86-64")
    arch = subprocess.check_output(["dpkg-deb", "-f", deb, "Architecture"], text=True).strip()
    require(arch == "amd64", "Expected the native amd64 Debian package")
    require(out.suffix == ".AppImage" and deb.stem == out.stem, "AppImage must use the Debian artifact stem")
    prefix = runtime()
    out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".kiwix-appimage-", dir=out.parent) as tmp:
        stage = Path(tmp)
        tree = stage / "AppDir"
        read_deb(deb, tree, binary, relay)
        plain = binary.read_bytes()
        require(plain.count(MARKER + b"UNK") == 1, "Expected a single Tauri bundle type placeholder")
        target = tree / BIN / APP
        require(target.is_file() and not target.is_symlink() and
                target.read_bytes() == plain.replace(MARKER + b"UNK", MARKER + b"DEB", 1),
                "Debian application differs from the exact native build")
        target.write_bytes(plain.replace(MARKER + b"UNK", MARKER + b"APP", 1))
        shutil.copyfile(TAURI / "linux/AppRun", tree / "AppRun")
        (tree / "AppRun").chmod(0o755)
        for name in LINKS:
            (tree / name).symlink_to(LINKS[name])
        notices = tree / RES / "licenses/appimage-runtime"
        notices.mkdir()
        for name in NOTICES:
            (notices / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(UPSTREAM / name, notices / name)
        tree.chmod(0o755)
        for path in tree.rglob("*"):
            if not path.is_symlink():
                path.chmod(0o755 if path.is_dir() or path.relative_to(tree) in
                           (Path("AppRun"), BIN / APP, BIN / "kiwix-mcp") else 0o644)
        check_tree(tree, binary, relay)
        env = dict(os.environ)
        epoch = env.pop("SOURCE_DATE_EPOCH", "0")
        require(epoch.isascii() and epoch.isdecimal() and int(epoch) < 2**32, "Invalid SOURCE_DATE_EPOCH")
        squash = stage / "payload"
        subprocess.run(["mksquashfs", tree, squash, "-noappend", "-nopad", "-no-progress",
                        "-no-xattrs", "-all-root", "-all-time", epoch, "-mkfs-time", epoch,
                        "-comp", "zstd"], env=env, check=True, stdout=subprocess.DEVNULL)
        result = stage / out.name
        with result.open("xb") as dest, squash.open("rb") as payload:
            dest.write(prefix)
            shutil.copyfileobj(payload, dest)
        result.chmod(0o755)
        inspect(result, binary, relay)
        os.replace(result, out)
        print(out)


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    actions = cli.add_subparsers(dest="action", required=True)
    build = actions.add_parser("assemble")
    for name in ("deb", "binary", "relay", "out"):
        build.add_argument(name, type=Path)
    audit = actions.add_parser("verify")
    audit.add_argument("image", type=Path)
    args = cli.parse_args()
    if args.action == "verify":
        inspect(args.image)
    else:
        assemble(args.deb, args.binary, args.relay, args.out)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError, KeyError) as error:
        sys.exit(f"Linux AppImage: {error}")
