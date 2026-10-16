#!/usr/bin/env python3
"""Bundles upstream model_specs/*.json into src/catalog/model_specs.json.

Usage: scripts/sync-catalog.py <audio.cpp checkout>

The bundle is minified and sorted by family so diffs between upstream
revisions stay reviewable with `jq`. The checkout must be at the commit
pinned in audio-cpp-sys/build.rs, which is recorded in the bundle.
"""
import json
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parent.parent
source = pathlib.Path(sys.argv[1]).resolve()

pinned = re.search(r'const SOURCE_COMMIT: &str = "([0-9a-f]{40})"', (root / "audio-cpp-sys/build.rs").read_text()).group(1)
try:
    actual = subprocess.run(["git", "-C", str(source), "rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
except (OSError, subprocess.CalledProcessError):
    actual = None
if actual is not None and actual != pinned:
    sys.exit(f"{source} is at {actual}, but build.rs pins {pinned}")

families = [json.loads(path.read_text(encoding="utf-8")) for path in sorted((source / "model_specs").glob("*.json"))]
families.sort(key=lambda spec: spec["family"])
bundle = {"source_commit": pinned, "families": families}
out = root / "src/catalog/model_specs.json"
out.write_text(json.dumps(bundle, ensure_ascii=False, separators=(",", ":"), sort_keys=True) + "\n", encoding="utf-8")
print(f"{len(families)} families -> {out.relative_to(root)} ({out.stat().st_size // 1024} KiB)")
