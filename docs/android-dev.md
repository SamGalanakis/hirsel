# Android development on this VM

This VM has a headless, hardware-accelerated Android toolchain at
`/workspace/android`. The repository recipes always source
`/workspace/android/env.sh`, so an agent can use the commands below without
editing its shell profile. The live checkout at `/workspace/code/hirsel` and
port `3089` are unrelated to this loop and must not be touched.

## Installed toolchain

The versions below were pinned and validated on 2026-07-10. Use these versions
when the Android app is scaffolded; do not substitute dynamic versions such as
`9.2.+`.

| Tool | Pinned version |
| --- | --- |
| Temurin JDK | 21.0.11+10 |
| Android command-line tools | 20.0 |
| Android SDK platform / build-tools | API 37.0 / 37.0.0 |
| Android platform-tools / emulator | 37.0.0 / 36.6.11 |
| Emulator system image | API 36 Google APIs x86_64, revision 7 |
| Android NDK LTS | r27d (`27.3.13750724`) |
| CMake | 4.1.2 |
| Rust toolchain / Android targets | 1.97.0 / `aarch64-linux-android`, `x86_64-linux-android` |
| cargo-ndk | 4.1.2 |
| Maestro CLI | 2.6.1 |

The AVD is `hirsel-emu`: a Pixel 6-class x86_64 device with 4 GB RAM, local
fast-boot snapshots, and a non-Play-Store Google APIs image. The latter matters:
`adb root` is available when native debugging eventually needs it.

To inspect tools directly:

```bash
source /workspace/android/env.sh
java -version
sdkmanager --version
emulator -version
adb version
rustup target list --installed
cargo ndk --version
maestro --version
```

## Fast path: boot, install, inspect, and capture

Run commands from this worktree:

```bash
cd /workspace/code/hirsel-android-dev
just emu
just emu-install /absolute/path/to/app-debug.apk
just emu-launch dev.hirsel.android
just emu-ui-dump
just emu-screenshot
just emu-logcat dev.hirsel.android
just emu-stop
```

`just emu` is idempotent and always targets `emulator-5554`. It prints exactly
`ready` after `sys.boot_completed=1`. A cold first boot normally takes 30-90
seconds and has a hard 180-second timeout. If the device is already ready, the
recipe reuses it and returns immediately. Emulator logs and the PID live under
`/tmp/hirsel-emu/`.

`just emu-stop` asks the emulator to shut down, waits up to 60 seconds, and
prints `stopped`. Calling it when the AVD is already down is safe.

## Recipe reference

All recipes use the fixed serial `emulator-5554`; they cannot accidentally
drive another attached phone.

### Screenshots

```bash
just emu-screenshot
# /tmp/hirsel-emu/screenshot-20260710T123456Z.png

just emu-screenshot /tmp/hirsel-emu/home.png
# /tmp/hirsel-emu/home.png
```

The command waits at most 20 seconds, writes through a temporary file, verifies
that the result is a non-empty PNG, and prints its absolute path. This is the
primary visual verification primitive for an agent. Read the returned file with
an image-viewing tool rather than inferring success from the exit status alone.

### Install and launch

```bash
just emu-install app/build/outputs/apk/debug/app-debug.apk
# Performing Streamed Install
# Success

just emu-launch dev.hirsel.android
# Starting: Intent { cmp=dev.hirsel.android/.MainActivity }
# Status: ok
```

Install uses `adb install -r` with a 120-second timeout, preserving app data on
replacement. Launch resolves the package's `MAIN`/`LAUNCHER` activity instead
of guessing its class name and waits at most 30 seconds. A missing APK or a
package without a launcher activity is reported as an error.

### Direct input

```bash
just emu-tap 540 1200
just emu-text "hello hirsel"
just emu-key BACK
just emu-key 3                 # numeric Android keyevent is also accepted
just emu-swipe 540 1800 540 600
```

These are thin, normally silent `adb shell input` wrappers with 10-second
timeouts. `emu-text` converts spaces for Android's input command and is intended
for simple ASCII. Prefer resource IDs, visible text, and Maestro for durable
automation; raw coordinates are a last-mile debugging tool.

### Find an element before tapping

```bash
just emu-ui-dump > /tmp/hirsel-emu/window.xml
rg 'text="Continue"|resource-id=".*continue"' /tmp/hirsel-emu/window.xml
```

The recipe waits at most 20 seconds and writes the compressed uiautomator XML
hierarchy to stdout. A node includes bounds such as
`bounds="[72,1040][1008,1176]"`. Tap its center:

```text
x = (72 + 1008) / 2 = 540
y = (1040 + 1176) / 2 = 1108
```

```bash
just emu-tap 540 1108
```

This find-by-text/resource-ID workflow is more reliable than guessing pixels.
Compose content must expose semantics before it appears in this hierarchy.

### Bounded logcat

```bash
just emu-logcat
just emu-logcat 'FATAL EXCEPTION|AndroidRuntime|dev.hirsel'
```

The recipe dumps only the most recent 500 log lines, waits at most 20 seconds,
and exits; it never follows indefinitely. The optional pattern is a
case-insensitive regular expression. No matches produce empty output rather
than an error.

## Maestro flows

Maestro is the preferred level above raw input commands. The future app should
keep committed flows under `android/.maestro/`; the initial convention is
`android/.maestro/smoke.yaml`:

```yaml
appId: dev.hirsel.android
---
- launchApp
- assertVisible: "Welcome to hirsel"
- tapOn: "Continue"
- assertVisible: "Inbox"
- takeScreenshot: smoke-home
```

Run one flow against the fixed emulator or the entire directory:

```bash
source /workspace/android/env.sh
maestro --device emulator-5554 test android/.maestro/smoke.yaml
maestro --device emulator-5554 test android/.maestro/
```

A passing flow ends with a green `Passed` result. Maestro waits for UI
conditions, selects by accessibility text/ID, records useful failure artifacts,
and supports `launchApp`, `tapOn`, `assertVisible`, `inputText`, and
`takeScreenshot` without coordinate math.

`maestro studio` is an interactive browser UI, not a CI/headless test runner.
The Android emulator may remain headless, but Studio itself needs a browser.
On this VM, start it with `maestro studio`, note the localhost URL, and reach it
through an SSH port forward or a remote browser. A failure to auto-open a local
browser is harmless. For unattended agents, use `maestro test <flow.yaml>` and
`just emu-ui-dump` instead; do not leave Studio running in the background.

## Host networking

Inside the Android emulator, `127.0.0.1` is the emulator itself. The host VM is
available at `10.0.2.2`. An app talking to a local hirsel development server on
port `3090`, for example, should use:

```text
http://10.0.2.2:3090
```

Use a free development port and never start or reconfigure port `3089` as part
of Android verification. Cleartext HTTP also needs an explicit Android network
security policy or `android:usesCleartextTraffic="true"` in development builds.

## Versions for the future app scaffold

The current stable, mutually compatible scaffold set validated here is:

- Android Gradle Plugin `9.2.1`
- Gradle wrapper `9.4.1`
- Kotlin / Compose compiler plugin `2.4.0`
- Compose BOM `2026.06.01`
- Activity Compose `1.13.0`
- `compileSdk = 37`, `targetSdk = 37`

AGP 9 has built-in Kotlin support. Do **not** apply
`org.jetbrains.kotlin.android`; apply only the Compose compiler plugin alongside
the Android plugin. A version catalog should begin like this:

```toml
[versions]
agp = "9.2.1"
kotlin = "2.4.0"
composeBom = "2026.06.01"
activityCompose = "1.13.0"

[libraries]
androidx-activity-compose = { module = "androidx.activity:activity-compose", version.ref = "activityCompose" }
androidx-compose-bom = { module = "androidx.compose:compose-bom", version.ref = "composeBom" }
androidx-compose-material3 = { module = "androidx.compose.material3:material3" }
androidx-compose-ui = { module = "androidx.compose.ui:ui" }
androidx-compose-ui-tooling-preview = { module = "androidx.compose.ui:ui-tooling-preview" }

[plugins]
android-application = { id = "com.android.application", version.ref = "agp" }
kotlin-compose = { id = "org.jetbrains.kotlin.plugin.compose", version.ref = "kotlin" }
```

Import the BOM as a platform in the app module:

```kotlin
plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

android {
    compileSdk = 37
    defaultConfig { targetSdk = 37 }
    buildFeatures { compose = true }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)
}
```

Reference sources: [AGP releases](https://developer.android.com/build/releases/gradle-plugin),
[built-in Kotlin migration](https://developer.android.com/build/migrate-to-built-in-kotlin),
[Kotlin releases](https://kotlinlang.org/docs/releases.html), and the
[Compose BOM guide](https://developer.android.com/develop/ui/compose/bom).

## Failure modes and recovery

- **`/dev/kvm is not accessible`**: `sam` belongs to the `kvm` group, but an
  already-open login may not have refreshed supplementary groups. `just emu`
  detects that case and launches through a detached `sg kvm` session; future
  logins use KVM directly. Confirm the temporary path with
  `sg kvm -c '. /workspace/android/env.sh && emulator -accel-check'`.
- **Boot timeout after 180 seconds**: inspect
  `/tmp/hirsel-emu/emulator.log`, run `just emu-stop`, then retry. Messages about
  unsupported nested virtualization or permission-denied KVM indicate a VM host
  problem, not an app problem.
- **`adb: device offline`**: wait a few seconds and rerun `just emu`; it reuses
  the in-progress process. If it remains offline past the boot timeout, stop and
  restart the AVD.
- **Snapshot will not restore**: stop the AVD cleanly once. For a one-time cold
  recovery, source the environment and launch the same fixed command with
  `-no-snapshot-load`, then shut it down cleanly. Do not delete the AVD as a
  first response.
- **Install fails with a downgrade/signature error**: uninstall that test
  package with `adb -s emulator-5554 uninstall <package>`, then reinstall. This
  intentionally destroys only that package's emulator data.
- **UI dump is empty or stale**: make sure the screen is unlocked, wait for the
  app to settle, and retry. Use `just emu-key WAKEUP` and
  `just emu-key MENU` if Android has slept.
- **Maestro cannot find a device**: verify `adb devices` contains exactly
  `emulator-5554 device`, then pass `--device emulator-5554` explicitly.
- **SDK XML version warning**: command-line tools 20 can print a non-fatal
  warning that it understands SDK XML through version 3 while the current
  repository contains version 4 metadata. The pinned packages still install
  and list correctly; revisit this only when Google publishes a newer stable
  command-line tools archive or an SDK operation actually fails.
