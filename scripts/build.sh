#!/usr/bin/env bash
# Build entry point for macOS and Linux; build.ps1 is the Windows one.
#
# The ordering is the same as build.ps1's, and it is load-bearing on a fresh clone:
#
# 1. The settings window is built before cargo runs. tauri::generate_context! reads
#    frontendDist while the lathe crate compiles, and beforeBuildCommand only fires
#    under `cargo tauri bundle`.
# 2. lathe-core is built first, alone, so CrispASR emits its shared libraries; they are
#    copied beside the executable before the full build, because tauri.conf.json ships
#    them as bundle resources and tauri-build resolves that glob while the lathe crate's
#    build script runs.
#
# Usage: ./scripts/build.sh [--dev] [--bundle] [cargo args...]

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dev=0
bundle=0
cargo_args=()
for arg in "$@"; do
    case "$arg" in
        --dev) dev=1 ;;
        --bundle) bundle=1 ;;
        *) cargo_args+=("$arg") ;;
    esac
done

need() {
    command -v "$1" >/dev/null 2>&1 || { echo "error: $1 not found; $2" >&2; exit 1; }
}
need cmake "install CMake (brew install cmake / apt install cmake)"
need ninja "install Ninja (brew install ninja / apt install ninja-build)"
need npm "install Node.js; the settings window is built with it"
export CMAKE_GENERATOR=Ninja

case "$(uname -s)" in
    Darwin)
        lib_glob='*.dylib'
        bundles=dmg
        ;;
    Linux)
        lib_glob='*.so*'
        bundles=deb,appimage
        # ggml's Vulkan backend compiles its shaders at build time.
        need glslc "install shaderc (apt install glslc)"
        ;;
    *)
        echo "error: use scripts/build.ps1 on Windows" >&2
        exit 1
        ;;
esac

cd "$root"

# The ${arr[@]+"${arr[@]}"} expansions below are for macOS's bash 3.2, where an empty
# array trips `set -u`.
if [ "$dev" = 1 ]; then
    profile_dir="$root/target/debug"
    profile_flag=()
else
    profile_dir="$root/target/release"
    profile_flag=(--release)
fi

# CrispASR builds its runtime as shared libraries inside the cargo OUT_DIR, and the
# executable will not start without them on its rpath. llama.cpp is linked statically
# precisely so its ggml does not produce files with the same names; see amendment A14.
copy_crispasr_runtime() {
    local build_dir
    build_dir="$(ls -d "$profile_dir"/build/crispasr-sys-*/out/crispasr-build 2>/dev/null | head -1 || true)"
    if [ -z "$build_dir" ]; then
        echo "warning: CrispASR build output not found; the binary will not start without its libraries" >&2
        return
    fi
    local copied=0
    for lib in "$build_dir"/src/$lib_glob "$build_dir"/ggml/src/$lib_glob; do
        [ -e "$lib" ] || continue
        # -a keeps the versioned symlinks (libggml.so -> libggml.so.0) intact.
        cp -a "$lib" "$profile_dir/"
        copied=$((copied + 1))
    done
    [ "$copied" -gt 0 ] && echo "copied $copied CrispASR runtime librar(ies) to $profile_dir"
    return 0
}

if [ ! -d "$root/ui/node_modules" ]; then
    npm --prefix "$root/ui" ci
fi
npm --prefix "$root/ui" run build

cargo build -p lathe-core ${profile_flag[@]+"${profile_flag[@]}"}
copy_crispasr_runtime

cargo build ${profile_flag[@]+"${profile_flag[@]}"} ${cargo_args[@]+"${cargo_args[@]}"}
copy_crispasr_runtime

if [ "$bundle" = 1 ]; then
    if [ "$dev" = 1 ]; then
        echo "error: --bundle needs a release build; drop --dev" >&2
        exit 1
    fi
    need cargo-tauri 'install it with: cargo install tauri-cli --version "^2" --locked'
    cargo tauri bundle --bundles "$bundles" --ci
fi
