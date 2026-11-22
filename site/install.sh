#!/bin/sh
set -u

# tools/toolchain-dist/build reads this assignment for the release gate.
glibc_floor=2.28
release_url=https://github.com/SamGalanakis/hirsel/releases
release_api=https://api.github.com/repos/SamGalanakis/hirsel/releases
target=x86_64-unknown-linux-gnu
modify_path=true
uninstall=false
download_dir=
stage=
pending=

fail() {
    printf 'hirsel installer: %s\n' "$*" >&2
    exit 1
}

for arg do
    case "$arg" in
        --no-modify-path) modify_path=false ;;
        --uninstall) uninstall=true ;;
        -h|--help)
            printf '%s\n' 'Usage: install.sh [--no-modify-path] [--uninstall]' \
                'Environment: HIRSEL_VERSION, HIRSEL_INSTALL_DIR, HIRSEL_DOWNLOAD_BASE'
            exit 0 ;;
        *) fail "unknown option $arg" ;;
    esac
done

: "${HOME:?set HOME before running the installer}"
prefix=${HIRSEL_INSTALL_DIR:-"$HOME/.hirsel"}
while [ "$prefix" != "${prefix%/}" ]; do
    prefix=${prefix%/}
done
case "$prefix" in
    /*) ;;
    *) fail 'HIRSEL_INSTALL_DIR must be an absolute path below /' ;;
esac
case "$prefix" in
    *:*|*'
'*) fail 'HIRSEL_INSTALL_DIR cannot contain a colon or newline' ;;
esac
# Quote literal paths for the shell startup files.
sh_quote() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

fish_quote() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/[\\\\']/\\\\&/g")"
}

if [ "$prefix" = "$HOME/.hirsel" ]; then
    # shellcheck disable=SC2016 # Expand HOME when the startup file is sourced.
    rc_line='[ -f "$HOME/.hirsel/env" ] && . "$HOME/.hirsel/env"'
else
    env_path=$(sh_quote "$prefix/env")
    rc_line="[ -f $env_path ] && . $env_path"
fi
fish_dir=${XDG_CONFIG_HOME:-"$HOME/.config"}/fish/conf.d
zsh_rc=${ZDOTDIR:-"$HOME"}/.zshrc

cleanup() {
    for directory in "$stage" "$pending" "$download_dir"; do
        if [ -n "$directory" ]; then rm -rf "$directory"; fi
    done
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

remove_rc_line() {
    [ -f "$1" ] || return 0
    if grep -Fxq -- "$rc_line" "$1"; then
        RC_LINE=$rc_line awk '$0 != ENVIRON["RC_LINE"]' "$1" > "$pending/rc" || fail "cannot read $1"
        cat "$pending/rc" > "$1" || fail "cannot update $1"
        rm "$pending/rc" || fail 'cannot remove temporary configuration'
    fi
}

if "$uninstall"; then
    if [ -L "$prefix/bin/hirsel" ] && [ "$(readlink "$prefix/bin/hirsel")" = '../current/hirsel' ]; then
        pending=$(mktemp -d "$prefix/.uninstall.XXXXXXXX") || fail 'cannot create uninstall directory'
        if "$modify_path"; then
            remove_rc_line "$HOME/.bashrc"
            remove_rc_line "$HOME/.profile"
            remove_rc_line "$zsh_rc"
            if [ -f "$fish_dir/hirsel.fish" ] && [ -f "$prefix/env.fish" ] &&
                cmp -s "$fish_dir/hirsel.fish" "$prefix/env.fish"; then
                rm -f "$fish_dir/hirsel.fish" || fail 'cannot remove fish configuration'
            fi
        fi
        if [ -L "$prefix/current" ]; then
            rm "$prefix/current" || fail 'cannot remove current version link'
        fi
        rm -f "$prefix/env" "$prefix/env.fish" || fail 'cannot remove environment files'
        rm -rf "$prefix/versions" || fail 'cannot remove installed versions'
        rm "$prefix/bin/hirsel" || fail 'cannot remove hirsel link'
        rmdir "$prefix/bin" "$pending" "$prefix" 2>/dev/null || :
    fi
    printf '%s\n' 'Hirsel uninstalled.'
    exit 0
fi

[ "$(uname -s)" = Linux ] || fail 'Hirsel requires Linux'
case "$(uname -m)" in
    x86_64) ;;
    *) fail 'Hirsel requires an x86_64 host' ;;
esac
libc=$(getconf GNU_LIBC_VERSION 2>/dev/null) || libc=
case "$libc" in
    'glibc '*) glibc=${libc#glibc } ;;
    *) fail "Hirsel requires glibc $glibc_floor or later (musl is unsupported)" ;;
esac
if ! awk -v host="$glibc" -v floor="$glibc_floor" 'BEGIN {
    if (host !~ /^[0-9]+\.[0-9]+(\.[0-9]+)?$/) exit 1
    split(host, a, "."); split(floor, b, ".")
    exit !(a[1]+0 > b[1]+0 || (a[1]+0 == b[1]+0 && a[2]+0 >= b[2]+0))
}'; then
    fail "Hirsel requires glibc $glibc_floor or later (found $glibc)"
fi

fetch() {
    case "$1" in
        # Stream to the calling shell so snap curl never opens a file itself.
        https://*)
            if command -v curl >/dev/null 2>&1 &&
                curl --proto '=https' --tlsv1.2 --retry 3 -fsSL "$1" > "$2"; then
                return 0
            fi
            command -v wget >/dev/null 2>&1 &&
                wget --https-only -qO- "$1" > "$2" ;;
        file:///*) cat "${1#file://}" > "$2" ;;
        *) return 1 ;;
    esac
}

release_page() {
    LC_ALL=C awk -v best="$1" '
        function compare_text(a, b) {
            return "x" a == "x" b ? 0 : ("x" a > "x" b ? 1 : -1)
        }
        function compare_number(a, b) {
            if (length(a) != length(b)) return length(a) - length(b)
            return compare_text(a, b)
        }
        function precedence(a, b,    a_pre, b_pre, av, bv, an, bn, i, order) {
            sub(/\+.*/, "", a); sub(/\+.*/, "", b)
            a_pre = index(a, "-"); b_pre = index(b, "-")
            # Once a stable release exists, only stable releases are eligible.
            if ((a_pre == 0) != (b_pre == 0)) return a_pre ? -1 : 1
            split(substr(a, 2), av, /[.-]/)
            split(substr(b, 2), bv, /[.-]/)
            for (i = 1; i <= 3; i++) {
                order = compare_number(av[i], bv[i])
                if (order) return order
            }
            if (!a_pre) return 0
            an = split(substr(a, a_pre + 1), av, /\./)
            bn = split(substr(b, b_pre + 1), bv, /\./)
            for (i = 1; i <= an && i <= bn; i++) {
                if (av[i] ~ digits && bv[i] ~ digits) order = compare_number(av[i], bv[i])
                else if (av[i] ~ digits) order = -1
                else if (bv[i] ~ digits) order = 1
                else order = compare_text(av[i], bv[i])
                if (order) return order
            }
            return an - bn
        }
        BEGIN {
            digits = "^[0-9]+$"
            number = "(0|[1-9][0-9]*)"
            pre = "(" number "|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
            build = "[0-9A-Za-z-]+"
            semver = "^v" number "\\." number "\\." number \
                "(-" pre "(\\." pre ")*)?([+]" build "(\\." build ")*)?$"
        }
        # Scan JSON strings whole so that only keys of a release object count.
        {
            while (match($0, /[^[:space:]]/)) {
                $0 = substr($0, RSTART)
                if (!match($0, /^("([^"\\]|\\.)*"|[][{}:,]|-?[0-9][0-9.eE+-]*|true|false|null)/)) {
                    invalid = 1; exit
                }
                token = substr($0, 1, RLENGTH)
                $0 = substr($0, RLENGTH + 1)
                if (closed || (!depth && token != "[") ||
                    (depth == 1 && token != "{" && token != "]" && token != ",")) {
                    invalid = 1; exit
                }
                if (depth == 1 && token == "{") count++
                if (depth == 2 && key == "\"tag_name\"" && previous == ":") {
                    tag = substr(token, 2, length(token) - 2)
                    if (token ~ /^"/ && tag ~ semver) {
                        order = best == "" ? 1 : precedence(tag, best)
                        if (order > 0 || (order == 0 && compare_text(tag, best) > 0)) best = tag
                    }
                }
                if (token == "[" || token == "{") depth++
                if (token == "]" || token == "}") depth--
                if (!depth) closed = 1
                key = previous
                previous = token
            }
        }
        END {
            if (invalid || !closed) exit 1
            printf "%d %s\n", count, best
        }
    ' "$download_dir/releases"
}

download_dir=$(mktemp -d "${TMPDIR:-/tmp}/hirsel-install.XXXXXXXX") || fail 'cannot create download directory'
version=${HIRSEL_VERSION:-}
if [ -z "$version" ]; then
    case ${HIRSEL_DOWNLOAD_BASE:-} in
        file://*) fail 'set HIRSEL_VERSION for local release artifacts' ;;
    esac
    page=1
    while :; do
        fetch "$release_api?per_page=100&page=$page" "$download_dir/releases" || fail 'cannot list GitHub releases'
        result=$(release_page "$version") || fail 'cannot read the GitHub releases list'
        version=${result#* }
        [ "${result%% *}" -eq 100 ] || break
        page=$((page + 1))
    done
    [ -n "$version" ] || fail 'no Hirsel release published yet'
fi
version=${version#v}
printf '%s\n' "$version" |
    LC_ALL=C awk 'NR != 1 || $0 !~ /^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?(\+[A-Za-z0-9.-]+)?$/ { exit 1 }' ||
    fail 'expected a release version such as v1.2.3'
base=${HIRSEL_DOWNLOAD_BASE:-"$release_url/download/v$version"}
archive=hirsel-v$version-$target.tar.gz
fetch "${base%/}/SHA256SUMS" "$download_dir/SHA256SUMS" || fail 'cannot download SHA256SUMS'
expected=$(LC_ALL=C awk -v asset="$archive" '
    $2 == asset || $2 == "*" asset {
        if (NF != 2 || $1 !~ /^[0-9a-fA-F]+$/ || length($1) != 64) exit 1
        digest = tolower($1); n++
    }
    END { if (n != 1) exit 1; print digest }
' "$download_dir/SHA256SUMS") || fail "SHA256SUMS must list exactly one valid digest for $archive"

if command -v sha256sum >/dev/null 2>&1; then
    hash_command() { sha256sum < "$1"; }
elif command -v shasum >/dev/null 2>&1; then
    hash_command() { shasum -a 256 < "$1"; }
elif command -v openssl >/dev/null 2>&1; then
    hash_command() { openssl dgst -sha256 -r < "$1"; }
else
    fail 'install sha256sum, shasum, or openssl to verify the release'
fi
fetch "${base%/}/$archive" "$download_dir/$archive" || fail "cannot download $archive"
actual=$(hash_command "$download_dir/$archive") || fail 'cannot compute SHA-256'
[ "${actual%% *}" = "$expected" ] || fail "SHA-256 mismatch for $archive"

has_bundle() {
    [ -x "$1/hirsel" ] && [ -x "$1/bin/ld.lld" ] &&
        [ -d "$1/link/kit" ] &&
        [ -f "$1/lib/libLLVM.so.23.1" ] && [ -f "$1/hirsel-core-toolchain.json" ]
}

if [ -L "$prefix/versions" ] || [ -L "$prefix/bin" ]; then
    fail 'versions and bin must be directories'
fi
mkdir -p "$prefix/versions" "$prefix/bin" || fail "cannot create $prefix"
mkdir "$prefix/versions/$version.tmp.$$" || fail 'cannot create staging directory'
stage=$prefix/versions/$version.tmp.$$
tar -xzf "$download_dir/$archive" -C "$stage" || fail 'cannot extract the release'
has_bundle "$stage" || fail 'release bundle is incomplete'
destination=$prefix/versions/$version
if [ ! -d "$destination" ]; then
    if ! mv -T "$stage" "$destination" 2>/dev/null; then
        if [ ! -d "$destination" ]; then
            mv "$stage" "$destination" || fail 'cannot publish the version directory'
            # If another installer won the race, plain mv can move inside its directory.
            stage=$destination/${stage##*/}
        fi
    fi
fi
has_bundle "$destination" || fail "existing version at $destination is incomplete"

pending=$(mktemp -d "$prefix/.install.XXXXXXXX") || fail 'cannot create publication directory'
[ ! -e "$prefix/current" ] || [ -L "$prefix/current" ] || fail 'current exists and is not a symbolic link'
# Keep ld.lld and the rest of the installation out of command lookup.
if ! ln -s ../current/hirsel "$prefix/bin" 2>/dev/null; then
    [ "$(readlink "$prefix/bin/hirsel")" = '../current/hirsel' ] || fail 'bin/hirsel points outside the installation'
fi

bin_path=$(sh_quote "$prefix/bin")
cat > "$pending/env" <<EOF
case ":\${PATH:-}:" in
    *:$bin_path:*) ;;
    *) export PATH=$bin_path\${PATH:+":\$PATH"} ;;
esac
EOF
[ ! -d "$prefix/env" ] || fail 'env is a directory'
mv -f "$pending/env" "$prefix/env" || fail 'cannot publish the environment file'
login_shell=${SHELL:-sh}
source_command=". $(sh_quote "$prefix/env")"
if [ "${login_shell##*/}" = fish ]; then
    printf 'fish_add_path --path %s\n' "$(fish_quote "$prefix/bin")" > "$prefix/env.fish" ||
        fail 'cannot write fish environment'
    source_command="source $(fish_quote "$prefix/env.fish")"
fi
ln -s "versions/$version" "$pending/current" || fail 'cannot stage current version link'
if ! mv -Tf "$pending/current" "$prefix/current" 2>/dev/null; then
    ln -sfn "versions/$version" "$prefix/current" || fail 'cannot set current version'
fi

if "$modify_path"; then
    case "${login_shell##*/}" in
        fish)
            mkdir -p "$fish_dir" || fail 'cannot create fish configuration directory'
            cp "$prefix/env.fish" "$fish_dir/hirsel.fish" || fail 'cannot configure fish'
            rc= ;;
        zsh) rc=$zsh_rc ;;
        bash) rc=$HOME/.bashrc ;;
        *) rc=$HOME/.profile ;;
    esac
    if [ -n "$rc" ]; then
        if [ ! -f "$rc" ] || ! grep -Fqx -- "$rc_line" "$rc"; then
            printf '\n%s\n' "$rc_line" >> "$rc" || fail "cannot update $rc"
        fi
    fi
    if [ -n "${GITHUB_PATH:-}" ]; then
        printf '%s\n' "$prefix/bin" >> "$GITHUB_PATH" || fail 'cannot update GITHUB_PATH'
    fi
fi
printf 'Installed Hirsel v%s. To use it now, run %s\n' "$version" "$source_command"
