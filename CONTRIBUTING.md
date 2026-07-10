# Contributing

Hirsel uses trunk-based development. Create a short-lived branch from an
up-to-date `main`, make a focused change, and open a pull request. The CI
workflow must pass before the branch is merged back to `main`; do not keep
long-running integration branches.

CI always checks the Rust workspace and web app. Its Android job runs only
when `android/**` or `crates/hirsel-client-*/**` changes. The Android job skips
without error when `GOOGLE_SERVICES_JSON_B64` is unavailable, as it is for
pull requests from forks.

## Local hooks with prek

Install [prek](https://github.com/j178/prek), then install both repository
hooks:

```bash
cargo install --locked prek
prek install --hook-type pre-commit
prek install --hook-type pre-push
```

The pre-commit stage runs generic file checks, the sensitive-path guard,
`cargo fmt`, oxlint, and TypeScript. The slower workspace-wide clippy check
runs on pre-push. Run the complete pre-commit set manually before opening a
pull request:

```bash
prek run --all-files
prek run cargo-clippy --all-files --stage pre-push
```

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
