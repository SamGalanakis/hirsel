#!/bin/bash
# Resolve the host toolchain delegates used by the nightly board under a minimal service PATH.

board_append_path() {
  local board_path=$1
  if [[ -d $board_path ]] && [[ ":$PATH:" != *":$board_path:"* ]]; then
    export PATH="$PATH:$board_path"
  fi
}

board_prepend_path() {
  local board_path=$1
  if [[ -d $board_path ]] && [[ ":$PATH:" != *":$board_path:"* ]]; then
    export PATH="$board_path:$PATH"
  fi
}

board_find_real_cargo() {
  local wrapper wrapper_resolved= candidate resolved cargo_home
  wrapper=$(command -v cargo 2>/dev/null || true)
  if [[ -n $wrapper ]]; then
    wrapper_resolved=$(readlink -f -- "$wrapper" 2>/dev/null || true)
  fi
  cargo_home=${CARGO_HOME:-$HOME/.cargo}
  for candidate in "$cargo_home/bin/cargo" "$HOME/.cargo/bin/cargo"; do
    [[ -x $candidate ]] || continue
    resolved=$(readlink -f -- "$candidate" 2>/dev/null || true)
    if [[ -n $resolved && ( -z $wrapper_resolved || $resolved != "$wrapper_resolved" ) ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

board_mise_tool() {
  local tool=$1 mise candidate
  mise=$(command -v mise 2>/dev/null || true)
  [[ -n $mise ]] || return 1
  candidate=$("$mise" which "$tool" 2>/dev/null) || return 1
  [[ -n $candidate && -x $candidate ]] || return 1
  printf '%s\n' "$candidate"
}

board_environment() {
  # The Kiln Cargo shim stays first; its rustup delegate and mise's toolchain
  # delegates are explicit, so neither shim depends on the service unit's PATH.
  board_append_path "$HOME/.local/bin"
  board_append_path "${CARGO_HOME:-$HOME/.cargo}/bin"
  board_append_path "$HOME/.cargo/bin"
  board_append_path "$HOME/.local/share/mise/shims"

  local cargo tool_path
  if cargo=$(board_find_real_cargo); then
    export KILN_REAL_CARGO=$cargo
  fi
  if tool_path=$(board_mise_tool node); then
    export BOARD_NODE=$tool_path
    board_prepend_path "${tool_path%/*}"
  fi
  if tool_path=$(board_mise_tool npm); then
    export BOARD_NPM=$tool_path
    board_prepend_path "${tool_path%/*}"
  fi
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  board_environment
  if (( $# )); then
    exec "$@"
  fi
fi
