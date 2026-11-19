#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Recreate the four offline PDF input snapshots from two article bodies and a stylesheet."""
import argparse
import json
from pathlib import Path
import re
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("repository", "output", "climate_html", "nollywood_html", "climate_css"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    args.output.mkdir()
    snapshots = json.loads(subprocess.check_output(
        ["node", "scripts/test-reader-live-export.mjs", "--audio-snapshots"], cwd=args.repository))
    if len(snapshots) != 2:
        raise ValueError("expected two bridge test snapshots")
    for i, snapshot in enumerate(snapshots):
        (args.output / f"bridge-{i}.html").write_text(snapshot["html"], encoding="utf-8")
    css = args.climate_css.read_text(encoding="utf-8")
    for name, path in [("climate", args.climate_html), ("nollywood", args.nollywood_html)]:
        source = path.read_text(encoding="utf-8")
        source = re.sub(r'(?i)(\s(?:src|poster|background))="[^"]*"', r'\1="data:image/png;base64,AAAA"', source)
        source = re.sub(r'(?i)\s(?:srcset|imagesrcset)="[^"]*"', "", source)
        source = re.sub(r'url\("\./_assets_/[^"]+"\)', 'url("data:image/png;base64,AAAA")', source)
        source = source.replace("</head>", "<style>" + css + "</style></head>")
        (args.output / (name + ".html")).write_text(source, encoding="utf-8")


if __name__ == "__main__":
    main()
