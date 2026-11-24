#!/bin/sh
# Run core/programs/sqlite/native_laws.c against the checked-in archive, in the
# 64 KiB tmpfs the launcher mounts (HIRSEL_TEST_SMALL_TMPFS).
set -eu
HIRSEL="$(realpath "$1")" SQLITE_HEADER="$(realpath "$SQLITE_HEADER")" \
    exec sh core/programs/sqlite/build.sh --test "${HIRSEL_TEST_SMALL_TMPFS_DIR:?the launcher mounts the small tmpfs}"
