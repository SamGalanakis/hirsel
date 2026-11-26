#!/bin/sh
# The toolchain's own Hirsel programs, for both bundle builds: Buck2's
# //:hirsel-toolchain (tools/buck2/defs.bzl) and a Mac host's
# `tools/toolchain-dist/build --host aarch64-apple-darwin` (darwin_host.py).
#
#   toolchain-programs.sh pack COMPILER SOURCES OUT WORK NAME...
#     Pack each core/programs/NAME under SOURCES alone as OUT/NAME-<major>.hpkg,
#     with the toolchain packages it binds bound from the toolchain, as an
#     installed package binds them, in place of the workspace's path bindings.
#
#   toolchain-programs.sh helpers ROOT PLATFORM WORK
#     Build the bundle ROOT's helpers with ROOT/hirsel from the platform
#     package's sources PLATFORM: the platform host libexec/hirsel/platform_main
#     (docs/spec/launch.md "Starting the host"), its offline pin reader
#     libexec/hirsel/pin_reader, and libexec/hirsel/store_tool, which runs
#     `hirsel store backup|restore` (STORE-SPEC §15.4) on the two directories the
#     driver passes as fds 3 and 4. Each is built for this host, so its directory
#     grants name this host's current directory and descriptor directory.
set -eu

# Every package a toolchain package binds is itself a toolchain package.
bind() {
    sed -E 's/^([a-z_]+) = \{ major = ([0-9]+), path = "[^"]*" \}$/\1 = { major = \2 }/' "$1"
}

# Copy a package's sources to $2. The copies keep the inputs' read-only mode
# under remote execution: replace the manifest rather than write through it.
place() {
    mkdir -p "$2"
    cp -RL "$1"/. "$2"/
    rm -f "$2/hirsel.toml"
    bind "$1/hirsel.toml" > "$2/hirsel.toml"
}

limit=1073741824
command="$1"
shift
case "$command" in
pack)
    compiler="$1"; sources="$2"; out="$3"; work="$4"; shift 4
    mkdir -p "$out" "$work"
    for name in "$@"; do
        place "$sources/$name" "$work/$name"
        major="$(sed -nE 's/^major = ([0-9]+)$/\1/p' "$work/$name/hirsel.toml" | head -n 1)"
        "$compiler" pack "$work/$name" --package-store "$work/packages" --output "$out/$name-$major.hpkg" > /dev/null
    done
    ;;
helpers)
    root="$1"; platform="$2"; work="$3"
    case "$(uname -s)" in
    Darwin) cwd=.; fd=/dev/fd ;;
    *) cwd=/proc/self/cwd; fd=/proc/self/fd ;;
    esac
    place "$platform" "$work/platform"
    mkdir -p "$root/libexec/hirsel"
    "$root/hirsel" build "$work/platform" --entry platform@1/platform_main/main \
        --grant Clock --grant Random --grant Tasks \
        --grant "Net=UnixRoot ctl $cwd;UnixConnect ctl launcher.sock;UnixListen ctl operator.sock" \
        --dir "$cwd/catalog" --limit "$limit" -o "$root/libexec/hirsel/platform_main" > /dev/null
    "$root/hirsel" build "$work/platform" --entry platform@1/platform_main/read_pin \
        --grant Clock --grant Stdout \
        --dir "$cwd/catalog" --limit "$limit" -o "$root/libexec/hirsel/pin_reader" > /dev/null
    "$root/hirsel" build "$work/platform" --entry platform@1/store_tool/main \
        --grant Clock --grant Random --grant Tasks --grant Stdout --grant Stderr \
        --dir "$fd/3" --dir "$fd/4" --limit "$limit" -o "$root/libexec/hirsel/store_tool" > /dev/null
    ;;
*)
    echo "toolchain-programs.sh: unknown command $command" >&2
    exit 2
    ;;
esac
