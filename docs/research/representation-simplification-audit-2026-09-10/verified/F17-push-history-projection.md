# F17 — concrete FCM JSON drops required history identity

Recommend; high confidence, high priority. Authoritative finding owner C22 Android contract; C25 push.rs conversion is consumer coordination. Worker ../workers/C22-FFI-ANDROID.md F1. Independently reopened PushData, coordinator construction, actual Firebase send_to_token JSON, Android service parser/notification Intent and notificationDestination routing. Exact identity query repeated:71 matches.

PushData already carries history_id, but push.rs179–183 manually constructs FCM data with only thread_id and title. HirselFirebaseMessagingService requires a nonblank history_id and returns before posting when absent. Thus the current host producer cannot satisfy its current Android consumer; notification tap routing also lacks the required history identity. No malformed external input is needed.

Target preserve captured history_id in the actual serialized FCM data map alongside decimal thread_id/title. Prefer one testable conversion helper from the existing typed destination to string-valued FCM data; do not add history defaults or weaken Android rejection. Scope push JSON projection plus a real serialized-body contract test, Android data/Intent parsing and valid/current vs stale-history routing assertions. Keep no-ID fallback removed. No DB/FFI API or broad push redesign. Audit ran no FCM/network/browser/device calls or tests.

Independent materiality qualification: the request also contains an FCM notification object. Source evidence establishes the app onMessageReceived callback and destination-routing failure, not that every foreground/background Android path posts no notification. Actual device delivery needs its platform fixture; the audit did not execute one.
