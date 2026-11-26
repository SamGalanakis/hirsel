#!/bin/bash
set -euo pipefail
keychain="$RUNNER_TEMP/hirsel-ci.keychain-db"
# This throwaway keychain holds no signing credentials.
security create-keychain -p '' "$keychain"
security set-keychain-settings "$keychain"
security unlock-keychain -p '' "$keychain"
security default-keychain -d user -s "$keychain"
