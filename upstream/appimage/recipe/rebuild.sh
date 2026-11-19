#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Rebuild and compare the retained Ubuntu x86-64 AppImage bootstrap.
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -f -- "$work/image-id" "$work/runtime"; rmdir "$work"' EXIT
docker build --platform linux/amd64 --iidfile "$work/image-id" -f "$root/recipe/Dockerfile" "$root"
docker run --rm --network none "$(cat "$work/image-id")" cat /upstream/out/runtime-x86_64 >"$work/runtime"
cmp "$work/runtime" "$root/runtime-x86_64"
sha256sum "$work/runtime"
