#!/usr/bin/env bash
# Moves the bindings to another audio.cpp commit.
#
#   scripts/update-upstream.sh [<commit>|<branch>|<tag>]
#
# A tag (v0.9.3) also becomes the pinned prebuilt release (PREBUILT_RELEASE and
# its archive digests, via scripts/pin-release.py);
# a branch or commit leaves the prebuilt release alone.
#
# Fetches that commit, copies its audiocpp.h, regenerates the bindings and the
# model catalog, and updates SOURCE_COMMIT in audio-cpp-sys/build.rs. Refuses
# a change of the ABI major version, which needs a human to read the diff.
# Requires git, python3, bindgen-cli and libclang.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
ref="${1:-main}"
repo="https://github.com/0xShug0/audio.cpp.git"

tag=""
if [[ "$ref" =~ ^[0-9a-f]{40}$ ]]; then
    commit="$ref"
elif commit="$(git ls-remote "$repo" "refs/tags/$ref^{}" | cut -f1)" && [[ -n "$commit" ]]; then
    tag="$ref"
else
    commit="$(git ls-remote "$repo" "refs/heads/$ref" | cut -f1)"
fi
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || { echo "cannot resolve $ref" >&2; exit 1; }

current="$(sed -n 's/^const SOURCE_COMMIT: &str = "\([0-9a-f]\{40\}\)";$/\1/p' "$root/audio-cpp-sys/build.rs")"
if [[ "$current" == "$commit" && -z "$tag" ]]; then
    echo "already at $commit"
    exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
git -C "$work" init -q
git -C "$work" fetch -q --depth 1 "$repo" "$commit"
git -C "$work" checkout -q --detach FETCH_HEAD

abi_major() { sed -n 's/^#define AUDIOCPP_ABI_VERSION_MAJOR \([0-9]*\)$/\1/p' "$1"; }
old_major="$(abi_major "$root/audio-cpp-sys/include/audiocpp.h")"
new_major="$(abi_major "$work/include/audiocpp.h")"
if [[ "$old_major" != "$new_major" ]]; then
    echo "ABI major changed ($old_major -> $new_major); update the safe wrapper by hand" >&2
    exit 2
fi

cp "$work/include/audiocpp.h" "$root/audio-cpp-sys/include/audiocpp.h"
sed -i.bak "s/^const SOURCE_COMMIT: &str = \"[0-9a-f]\{40\}\";$/const SOURCE_COMMIT: \&str = \"$commit\";/" "$root/audio-cpp-sys/build.rs"
rm -f "$root/audio-cpp-sys/build.rs.bak"
"$root/scripts/gen-bindings.sh"
python3 "$root/scripts/sync-catalog.py" "$work"
if [[ -n "$tag" ]]; then
    python3 "$root/scripts/pin-release.py" "$tag"
fi
echo "updated ${current:0:12} -> ${commit:0:12}"
