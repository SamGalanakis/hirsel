# Contributing

## Way of working

Use [GitHub issues in SamGalanakis/hirsel](https://github.com/SamGalanakis/hirsel/issues)
as the source of truth for Hirsel task tracking. Track bugs, planned improvements,
and follow-up work there. Check existing issues before proposing or filing new
work, and link pull requests to the issues they address. Linear is not Hirsel's
task tracker.

Hirsel uses trunk-based development. Create a short-lived branch from an
up-to-date `main`, make a focused change, and open a pull request. The CI
workflow must pass before the branch is merged back to `main`; do not keep
long-running integration branches.

The Owner has authorized routine pushes and keeping the local test instance
current after validated changes. Push completed commits with a normal
fast-forward push, then refresh the test app at `http://127.0.0.1:3076` when
its code has changed. Build and verify a matching backend/frontend release
before restarting; preserve its current configuration, history, data, and
previous release. Identify the current listener and check for active work
instead of reusing a recorded PID or an old cutover script. Verify liveness,
readiness, served assets, and the authenticated connection after the update.
This is a completed-change workflow, not a restart on every file save.

CI always checks the Rust workspace and web app. Its Android job builds when
Android, native client, protocol, Cargo, or CI workflow files change. It uses
a placeholder Firebase configuration, so pull requests do not need release
secrets to compile the debug APK.

## Local hooks with prek

Use the Node version pinned in CI and the npm version in `app/package.json`.
Run `npm ci` in `app/` after dependency changes so local validation checks the
same lockfile contract as CI.

Install [prek](https://github.com/j178/prek), then install both repository
hooks:

```bash
cargo install --locked prek
prek install --hook-type pre-commit
prek install --hook-type pre-push
```

The hooks run generic file checks, the sensitive-path guard, and
`scripts/check-static.sh`: source-size and plugin-sync checks, Rust formatting
and workspace clippy, web lint, and TypeScript. Run the checks and Rust tests
before opening a pull request:

```bash
prek run --all-files
cargo test --workspace
```

## Product runbooks

Behavior visible to the Owner is accepted with the agent-judged scenarios in
[`runbooks/`](runbooks/). They boot disposable Host data and the production web
build, use the configured real model path, and preserve browser/wire/store
evidence. Read [`runbooks/RULES.md`](runbooks/RULES.md) before running one:

```bash
just product-runbook all
```

The deterministic Rust, frontend, and `e2e/*.mjs` gates remain necessary but
do not replace these product checks.

## Releases

Android releases are manual. Run the release workflow from `main` with a
semantic version tag:

```bash
gh workflow run release.yml -f version=vX.Y.Z
```

Alternatively, pushing a matching tag starts the same workflow:

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

The workflow regenerates both Android native libraries and the UniFFI Kotlin
binding from Rust source, builds a signed release APK, uploads it as a workflow
artifact, and attaches it to a GitHub Release. If no release keystore is
configured, the APK uses debug signing and remains installable. Configure a
stable release key so a later APK can update an existing installation without
uninstalling it. Preserve that keystore and its credentials permanently.

A GitHub Pages deployment for the web PWA is a possible future addition, but
it is not part of the release workflow.

## Required GitHub secrets

Run these commands from this repository. The Firebase Android configuration is
required for release builds and for the path-filtered Android CI job:

```bash
gh secret set GOOGLE_SERVICES_JSON_B64 --body "$(base64 -w0 /workspace/secrets/google-services.json)"
```

`GOOGLE_SERVICES_JSON_B64` is the single-line output of:

```bash
base64 -w0 /workspace/secrets/google-services.json
```

The four signing secrets below are optional as a group. Generate a stable
keystore once (the command prompts for its passwords and certificate details):

```bash
keytool -genkeypair -v \
  -keystore /workspace/secrets/hirsel-release.jks \
  -storetype JKS \
  -alias hirsel \
  -keyalg RSA \
  -keysize 4096 \
  -validity 10000
```

Set all four signing secrets, replacing the alias only if a different one was
used during generation:

```bash
gh secret set SIGNING_KEYSTORE_B64 --body "$(base64 -w0 /workspace/secrets/hirsel-release.jks)"

read -rsp 'Keystore password: ' HIRSEL_KEYSTORE_PASSWORD; echo
gh secret set SIGNING_KEYSTORE_PASSWORD --body "$HIRSEL_KEYSTORE_PASSWORD"
unset HIRSEL_KEYSTORE_PASSWORD

gh secret set SIGNING_KEY_ALIAS --body 'hirsel'

read -rsp 'Key password: ' HIRSEL_KEY_PASSWORD; echo
gh secret set SIGNING_KEY_PASSWORD --body "$HIRSEL_KEY_PASSWORD"
unset HIRSEL_KEY_PASSWORD
```

`FCM_SERVICE_ACCOUNT_B64` is not needed to build an APK and is intentionally
not wired into either workflow. If a future release step sends a test push,
set it then with:

```bash
gh secret set FCM_SERVICE_ACCOUNT_B64 --body "$(base64 -w0 /workspace/secrets/fcm-service-account.json)"
```
