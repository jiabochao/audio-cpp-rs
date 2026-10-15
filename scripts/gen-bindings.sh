#!/usr/bin/env bash
# Regenerates audio-cpp-sys/src/bindings.rs from the vendored audiocpp.h.
# Requires bindgen-cli (`cargo install bindgen-cli`) and libclang.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
bindgen "$root/audio-cpp-sys/audio.cpp/include/audiocpp.h" \
    --allowlist-function 'audiocpp_.*' \
    --allowlist-type 'audiocpp_.*' \
    --allowlist-var 'AUDIOCPP_.*' \
    --no-prepend-enum-name \
    --no-layout-tests \
    --no-doc-comments \
    --use-core \
    --rust-target 1.85 \
    --rust-edition 2024 \
    -o "$root/audio-cpp-sys/src/bindings.rs"
