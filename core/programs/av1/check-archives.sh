#!/bin/bash
set -euo pipefail
directory=${1:-$(dirname "$0")}
llvm=${LLVM_SYS_231_PREFIX:?set LLVM_SYS_231_PREFIX to the pinned LLVM tree}

check_archive() {
    local archive=$1 prefix=$2 sections symbols exports relocations
    sections=$("$llvm/bin/llvm-objdump" --section-headers "$archive")
    symbols=$(readelf --wide --syms "$archive")
    relocations=$(readelf --wide --relocs "$archive")
    exports=$("$llvm/bin/llvm-nm" --defined-only --extern-only --just-symbol-name "$archive" | grep -v -e '^$' -e ':$')

    if [[ $sections =~ [[:space:]]\.(init|fini|preinit_array|init_array|fini_array|ctors|dtors)([.[:space:]]) ]] ||
       [[ $symbols =~ [[:space:]](IFUNC|UNIQUE)[[:space:]] ]] ||
       [[ $relocations == *IRELATIVE* ]] ||
       [[ $symbols =~ [[:space:]](dlopen|dlmopen|atexit|__cxa_atexit)([[:space:]@]|$) ]]; then
        echo "$archive contains forbidden ELF features or dynamic code loading" >&2
        return 1
    fi
    [[ -n $exports ]]
    while IFS= read -r symbol; do
        [[ $symbol == "$prefix"* ]] || { echo "$archive exports $symbol" >&2; return 1; }
    done <<< "$exports"
}

check_archive "$directory/libhirsel_dav1d.a" hdav_
check_archive "$directory/libhirsel_svtav1.a" hsvt_
