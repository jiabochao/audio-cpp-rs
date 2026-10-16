#!/usr/bin/env python3
"""Pins a prebuilt release in audio-cpp-sys/build.rs.

Usage: scripts/pin-release.py <tag>

Writes PREBUILT_RELEASE and the name and sha256 of every library archive of
that release into build.rs, so building against the pinned release needs no
GitHub API call (anonymous calls are rate limited per IP, which CI runners
and offices share) and verifies archives against digests recorded here
rather than ones fetched at build time.
"""
import json
import os
import pathlib
import re
import sys
import urllib.request

tag = sys.argv[1]
root = pathlib.Path(__file__).resolve().parent.parent
build_rs = root / "audio-cpp-sys/build.rs"

request = urllib.request.Request(
    f"https://api.github.com/repos/0xShug0/audio.cpp/releases/tags/{tag}",
    headers={"Accept": "application/vnd.github+json", "User-Agent": "audio-cpp-rs-pin"},
)
if os.environ.get("GITHUB_TOKEN"):
    request.add_header("Authorization", f"Bearer {os.environ['GITHUB_TOKEN']}")
release = json.load(urllib.request.urlopen(request))

rows = []
for asset in sorted(release["assets"], key=lambda a: a["name"]):
    name = asset["name"]
    if not re.match(rf"audio-{re.escape(tag)}-(lib|cudart)-", name):
        continue
    digest = (asset.get("digest") or "").removeprefix("sha256:")
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        sys.exit(f"{name} has no sha256 digest")
    rows.append(f'    ("{name}", "{digest}"),')
if not any("-lib-" in row for row in rows):
    sys.exit(f"release {tag} has no -lib- archives")

table = "\n".join(rows)
text = build_rs.read_text()
text = re.sub(r'const PREBUILT_RELEASE: &str = "[^"]*";', f'const PREBUILT_RELEASE: &str = "{tag}";', text)
text, count = re.subn(
    r"(// BEGIN PREBUILT_ASSETS[^\n]*\n).*?( *// END PREBUILT_ASSETS)",
    lambda m: m.group(1) + table + "\n" + m.group(2),
    text,
    flags=re.S,
)
if count != 1:
    sys.exit("PREBUILT_ASSETS markers not found in build.rs")
build_rs.write_text(text)
print(f"pinned {tag}: {len(rows)} archives")
