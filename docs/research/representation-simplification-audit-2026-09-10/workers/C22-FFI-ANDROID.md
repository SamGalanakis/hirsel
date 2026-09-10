# C22-FFI-ANDROID combined audit

## Scope, authority, and snapshot

This is the mechanically verifiable, read-only C22 audit for the UniFFI
contract, generated Kotlin, Android UI, pairing, settings, and notification
boundary. The assigned worker was gpt-5.6-luna at max effort. I used the
schemasmash and audit-your-codebase lenses and the native Android audit
reference. No source edits, tests, builds, installs, migrations, commits,
provider calls, live-data reads, or session/process actions were performed.

The pre-audit checks were:

~~~text
git rev-parse HEAD HEAD^{tree}
3ee0621a603659ab0168f565b99012b642415419
a4aac830c45398a66591f2c44b707aaf3cef281b

git status --porcelain
[empty]
~~~

The assigned report path was absent before this report was created. The
adjacent host push implementation is cited only as read-only producer context;
it is not treated as a C22-owned file or as a second finding owner.

## Implementation integrity verdict

FAIL for the current push delivery boundary: the Android consumer requires a
store-history identity that the current FCM wire projection omits, so the
current host-produced message cannot complete the Android notification route.
There is one additional P2 token-lifecycle gap. The UniFFI record/converter
surface, pairing path, Thread tree navigation, and native UI structure are
otherwise coherent from source inspection.

Findings are deliberately capped at two:

| Severity | Finding | Confidence |
|---|---|---|
| P1 | F1. The FCM wire projection drops required history identity | High |
| P2 | F2. FCM token refresh is not registered as a state change | High |

No P0 or P3 finding is reported.

## Audit health score

This is a source-level native-app quality score, not a claim that an emulator
or test suite was run.

| # | Dimension | Score | Key finding |
|---|---|---:|---|
| 1 | Accessibility | 3/4 | Compose controls have labels, roles/test tags, semantic descriptions, and sp-based text; runtime large-font traversal was not executed. |
| 2 | Performance | 3/4 | Lazy/list-oriented UI and IO-dispatched registration are present; no profiler or runtime measurement was performed. |
| 3 | Appearance & Theming | 3/4 | Explicit light/dark semantic color tokens and Material 3 are used; the product palette is static rather than Dynamic Color. |
| 4 | Platform Conformance | 3/4 | The surface is native Compose with edge-to-edge and inset handling; the push contract has a functional boundary failure. |
| 5 | Adaptivity | 3/4 | The inspected layouts use Compose sizing/insets and IME padding; no material size-class defect was verified for the unspecified target range. |
| **Total** |  | **15/20** | **Good, with the P1 push contract fixed before release.** |

### Platform conformance verdict

PASS as a native Android surface, not a ported website. The implementation uses
Compose and Material 3 controls, supports system-bar insets and IME padding,
and keeps system Back in the activity/navigation flow. F1 and F2 are
cross-layer delivery/lifecycle defects, not evidence of an off-platform
navigation or component model.

## Finding F1 — P1: the FCM wire projection drops required history identity

### Verdict and impact

The current Thread push payload is internally populated with history_id, but
the concrete FCM JSON conversion sends only thread_id and title. The Android
service now requires history_id and returns before posting a notification when
it is absent. A message produced by the current host path therefore cannot
complete the foreground notification path. In normal Android background
handling, a notification tap also reaches MainActivity without the required
history extra and is rejected as unavailable. This is a reachable P1 release
defect, not a hypothetical malformed-input case.

The strict Android behavior and lossy producer projection were both introduced
at the expected HEAD. The host path is adjacent read-only context; the
Android-side contract and rejection are C22-owned.

### Exact representations and conversions

The product contract explicitly requires history identity to travel with the
Thread ID:

docs/android-dev.md:40-44

~~~text
The shared Rust core and FFI require explicit nullable parents on creation and
explicit artifact reference arrays on submission. Notifications and retry state
carry the store history identity as well as the Thread ID. An empty store has no
reserved Thread; retained ID 0 is an ordinary root. A changed history invalidates
old recipients and runtime state while allowing plain draft text recovery.
~~~

The adjacent producer type retains all three values:

crates/hirsel-host/src/push.rs:31-36

~~~rust
pub struct PushData {
    pub history_id: String,
    pub thread_id: u64,
    pub title: String,
}
~~~

The eligible producer path obtains history_id, loads tokens, and constructs
PushData with it:

crates/hirsel-host/src/push.rs:308-348

~~~rust
pub(crate) async fn enqueue_thread(&self, thread: &Thread) {
    let Ok(history_id) = self.storage.history_id().await else {
        return;
    };
    let eligible = thread.attention == ThreadAttention::NeedsOwner
        && thread.settled_at.is_none()
        && thread.archived_at.is_none()
        && thread.snoozed_until.is_none_or(|until| until <= Utc::now());
    let Some(delivery) = self.claim_delivery(&history_id, thread.id, eligible) else {
        return;
    };

    let tokens = match self.storage.push_tokens().await {
        Ok(tokens) => tokens
            .into_iter()
            .map(|registered| registered.token)
            .collect::<Vec<_>>(),
        Err(error) => {
            tracing::warn!(thread_id = thread.id, %error, "failed to load push tokens");
            self.release_delivery(delivery);
            return;
        }
    };
    if tokens.is_empty() {
        self.release_delivery(delivery);
        return;
    }

    let payload = PushPayload {
        title: OWNER_APP_NAME.to_string(),
        body: if thread.description.trim().is_empty() {
            thread.title.clone()
        } else {
            thread.description.clone()
        },
        data: PushData {
            history_id: history_id.clone(),
            thread_id: thread.id,
            title: thread.title.clone(),
        },
    };
~~~

The FCM conversion is the lossy layer:

crates/hirsel-host/src/push.rs:172-184

~~~rust
.json(&serde_json::json!({
    "message": {
        "token": token,
        "notification": {
            "title": payload.title,
            "body": payload.body,
        },
        "data": {
            "thread_id": payload.data.thread_id.to_string(),
            "title": payload.data.title,
        }
    }
}))
~~~

There is no history_id entry in the emitted data object even though the source
PushData value is populated.

The C22-owned Android parser requires the omitted key:

android/app/src/main/kotlin/dev/hirsel/android/HirselFirebaseMessagingService.kt:24-35

~~~kotlin
override fun onMessageReceived(message: RemoteMessage) {
    val name = message.data["title"] ?: return
    val threadId = message.data["thread_id"]?.takeIf { it.toULongOrNull() != null } ?: return
    val historyId = message.data["history_id"]?.takeIf { it.isNotBlank() } ?: return
    val title = message.notification?.title ?: "Hirsel"
    val body = message.notification?.body ?: name
    if (!SettingsStore(this).pushEnabled) {
        Log.i(FCM_LOG_TAG, "push disabled in settings; suppressing notification for Thread $threadId")
        return
    }
    postThreadNotification(title, body, name, threadId, historyId)
}
~~~

The service puts the required value into the tap intent:

android/app/src/main/kotlin/dev/hirsel/android/HirselFirebaseMessagingService.kt:49-53

~~~kotlin
val launchIntent = Intent(this, MainActivity::class.java).apply {
    flags = Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP
    putExtra("thread_id", threadId)
    putExtra("history_id", historyId)
}
~~~

MainActivity reads both extras:

android/app/src/main/kotlin/dev/hirsel/android/MainActivity.kt:89-102

~~~kotlin
class MainActivity : ComponentActivity() {
    private var notificationHistoryId by mutableStateOf<String?>(null)
    private var notificationThreadId by mutableStateOf<ULong?>(null)

    override fun onNewIntent(intent: android.content.Intent) {
        super.onNewIntent(intent)
        notificationThreadId = intent.getStringExtra("thread_id")?.toULongOrNull()
        notificationHistoryId = intent.getStringExtra("history_id")
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        notificationThreadId = intent.getStringExtra("thread_id")?.toULongOrNull()
        notificationHistoryId = intent.getStringExtra("history_id")
~~~

The route effect passes the pair to the destination guard:

android/app/src/main/kotlin/dev/hirsel/android/MainActivity.kt:190-198

~~~kotlin
val connection = rememberConnection(activeSpec)
LaunchedEffect(notificationThreadId, notificationHistoryId, connection.snapshot?.historyId, connection.phase) {
    val id = notificationThreadId
    if (id != null && connection.isOnline && connection.snapshot?.historyId != null) {
        val destination = notificationDestination(notificationHistoryId, id, connection.snapshot?.historyId, connection.snapshot?.threads.orEmpty().map { it.id })
        if (destination != null) connection.openThread(destination)
        else connection.actionError = "This Thread is no longer available."
        onNotificationHandled()
    }
}
~~~

The actual destination conversion intentionally rejects a missing or stale
history:

android/app/src/main/kotlin/dev/hirsel/android/NotificationDestination.kt:3-11

~~~kotlin
/** A numeric Thread ID is meaningful only in the exact history that issued the push. */
internal fun notificationDestination(
    notifiedHistoryId: String?,
    notifiedThreadId: ULong?,
    currentHistoryId: String?,
    availableThreadIds: Collection<ULong>,
): ULong? = notifiedThreadId?.takeIf {
    !notifiedHistoryId.isNullOrBlank() && notifiedHistoryId == currentHistoryId && it in availableThreadIds
}
~~~

The push data does not pass through the UniFFI record converters. The FFI
surface only exposes the separate token-registration command:

crates/hirsel-client-ffi/src/lib.rs:478-482

~~~rust
pub fn register_push_token(&self, platform: String, token: String) -> Result<(), ClientError> {
    self.core
        .register_push_token(platform, token)
        .map_err(Into::into)
}
~~~

That is ownership evidence: the correction belongs at the host FCM
serialization and Android notification boundary, not in generated Kotlin or
the Rust snapshot converters.

### Concrete invalid state and reachability

The invalid combination is:

~~~text
PushData.history_id = H
PushData.thread_id = N
FCM data = { "thread_id": "N", "title": "..." }   // H was dropped
Android onMessageReceived requires message.data["history_id"] != blank
~~~

It is reachable whenever PushGateway.enqueue_thread receives a Thread with
NeedsOwner attention, an unsettled/unarchived/unsnoozed state, a readable store
history, and at least one registered push token. The producer constructs H at
push.rs:308-347 and the FcmPushSender conversion at push.rs:161-187 emits the
incomplete map. When that message reaches the Android service, line 27 returns
before line 34 can post the notification.

The numeric Thread ID is intentionally not sufficient: the destination helper
and Android development contract both require matching history because an ID
can be reused across store histories.

### Duplicate-truth assessment

No duplicate-truth write path was found inside the owned C22 code. This is a
lossy conversion defect, not two writers racing over one field. One source
representation contains history_id and the wire projection updates the other
representation without it. The resulting source/wire mismatch is the
cross-layer invariant failure.

### Reproducible consumer query

From the repository root:

~~~bash
rg -n 'history_id|historyId|thread_id|threadId|notificationDestination|postThreadNotification' android/app/src/main/kotlin/dev/hirsel/android crates/hirsel-host/src/push.rs
~~~

This returned 71 matching lines in the audited snapshot. The relevant consumer
chain is the host PushData construction and JSON map, the Android service
parser, MainActivity intent/snapshot routing, and
notificationDestination.

### Exact target representation

The smallest coherent target is to preserve one typed ThreadPushDestination
contract through the wire boundary:

~~~text
Host PushData:
    history_id: String
    thread_id: u64
    title: String

FCM data object:
    {
      "history_id": non-empty String,
      "thread_id": decimal String,
      "title": String
    }

Android parsed value:
    ThreadPushDestination(
        historyId: String,
        threadId: ULong,
        title: String,
    )
~~~

The host should serialize all three existing PushData fields. The Android
service should parse the three fields once into the typed destination and pass
that value to notification construction. MainActivity and
notificationDestination should continue to receive the same history/thread
pair. No database DDL change and no UniFFI converter change is required.

This removes the representable state in which the host object has a history
identity but the wire message does not. It also prevents a future field-by-field
consumer call from reconstructing a destination without its identity.

### Smallest credible affected files and ownership

- Adjacent producer owner: crates/hirsel-host/src/push.rs, specifically the
  FCM data JSON projection at lines 161-187.
- C22 Android owner: android/app/src/main/kotlin/dev/hirsel/android/HirselFirebaseMessagingService.kt.
- Existing C22 route/guard: MainActivity.kt and NotificationDestination.kt;
  retain the guard.
- Validation: a producer wire-shape fixture and an Android service/destination
  contract test. Do not edit generated Kotlin for this change.

The coordinator should sequence the host serialization fix with the Android
consumer contract test, while keeping the older-payload rejection behavior
intentional because a missing history cannot be safely navigated.

### Regression and cutover risk

The direct fix is low-risk because it adds an already-populated field to the
existing FCM data object. The main risk is a producer/consumer rollout in
which an old producer still emits event-shaped payloads or a test fixture
asserts the old key set. Those messages should remain rejected rather than
being routed by Thread ID alone. Existing pending notifications without a
history identity cannot be made safe by an Android fallback.

### Existing and additional validation

Existing C22 tests demonstrate only the defensive half:

android/app/src/test/kotlin/dev/hirsel/android/NotificationDestinationTest.kt:8-20

~~~kotlin
@Test fun rejectsReusedNumericIdFromPriorHistory() {
    assertNull(notificationDestination("old", 1uL, "new", listOf(1uL)))
    assertEquals(1uL, notificationDestination("new", 1uL, "new", listOf(1uL)))
}
@Test fun rejectsMissingHistoryAndUnavailableThread() {
    assertNull(notificationDestination(null, 1uL, "new", listOf(1uL)))
    assertNull(notificationDestination("new", 1uL, null, listOf(1uL)))
    assertNull(notificationDestination("new", 1uL, "new", listOf(2uL)))
}
@Test fun zeroIsAnOrdinaryExplicitDestination() {
    assertEquals(0uL, notificationDestination("current", 0uL, "current", listOf(0uL)))
    assertNull(notificationDestination("current", null, "current", listOf(0uL)))
}
~~~

These tests demonstrate that missing/stale history is rejected and that zero
is an ordinary destination. They do not demonstrate FCM JSON serialization or
delivery. The host push tests use recording/fake senders and exercise retry and
deduplication; they do not assert the real FCM JSON data map. No tests were
executed in this audit.

Additional validation required later, but not run here:

- Assert the real FCM JSON conversion contains history_id, thread_id, and
  title, with all values represented as FCM-compatible strings.
- Feed the exact serialized data map into the Android parser and assert that a
  matching history/thread produces a notification intent with both extras.
- Assert that absent or mismatched history still produces no unsafe
  destination.
- Exercise a changed store history and retained Thread ID 0 in the same
  contract fixture.

Confidence: high.

## Finding F2 — P2: FCM token refresh is not registered as a state change

### Verdict and impact

The Android app registers the FCM token only from a Compose effect keyed by
connection.isOnline. The Firebase refresh callback logs the new token and
does not persist or register it. If FCM changes T1 to T2 while the connection
remains online, the app has a current token T2 but the host registry still
contains T1. Push delivery can silently stop until a later activity
recreation/reconnection causes fetchFcmToken to run again. This is a reachable
P2 lifecycle defect with a normal workaround, but no guaranteed timely repair.

The same missing reactive state means changing the local push preference does
not itself retrigger the registration effect while HirselRoot remains composed.
The local service still suppresses delivery when disabled, so this is an
amplifier of the token-registration gap rather than a separately counted
finding.

### Exact representations and conversions

The refresh callback has no state or transport side effect:

android/app/src/main/kotlin/dev/hirsel/android/HirselFirebaseMessagingService.kt:19-22

~~~kotlin
class HirselFirebaseMessagingService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        Log.i(FCM_LOG_TAG, "FCM token refreshed")
    }
}
~~~

The only registration effect is keyed on connection.isOnline:

android/app/src/main/kotlin/dev/hirsel/android/MainActivity.kt:225-235

~~~kotlin
// Best-effort FCM registration once the transport is up (Thread push tokens),
// gated on the user's push preference.
LaunchedEffect(connection.isOnline) {
    if (!connection.isOnline || !settings.pushEnabled) return@LaunchedEffect
    runCatching {
        val token = fetchFcmToken()
        Log.i(FCM_LOG_TAG, "FCM token fetched")
        withContext(Dispatchers.IO) { connection.client?.registerPushToken("android", token) }
        Log.i(FCM_LOG_TAG, "FCM token registered with Hirsel host")
    }.onFailure { Log.e(FCM_LOG_TAG, "FCM token registration failed", it) }
}
~~~

fetchFcmToken is called only from that effect:

android/app/src/main/kotlin/dev/hirsel/android/MainActivity.kt:606-621

~~~kotlin
private suspend fun fetchFcmToken(): String = suspendCoroutine { continuation ->
    FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
        if (!task.isSuccessful) {
            continuation.resumeWithException(
                task.exception ?: IllegalStateException("Firebase token retrieval failed"),
            )
            return@addOnCompleteListener
        }
        val token = task.result
        if (token.isNullOrBlank()) {
            continuation.resumeWithException(IllegalStateException("Firebase returned an empty FCM token"))
        } else {
            continuation.resume(token)
        }
    }
}
~~~

The settings control writes a preference and local screen state, but does not
send an event to the root registration effect:

android/app/src/main/kotlin/dev/hirsel/android/SettingsScreen.kt:199-204

~~~kotlin
ToggleRow(
    title = "Push notifications",
    subtitle = "Register this device for Thread notifications.",
    checked = pushEnabled,
    onCheckedChange = { pushEnabled = it; settings.pushEnabled = it },
    testTag = "push-toggle",
)
~~~

android/app/src/main/kotlin/dev/hirsel/android/settings/SettingsStore.kt:23-25

~~~kotlin
var pushEnabled: Boolean
    get() = prefs.getBoolean(KEY_PUSH, true)
    set(value) { prefs.edit().putBoolean(KEY_PUSH, value).apply() }
~~~

The client transport accepts a token and queues it, but has no Android
lifecycle state:

crates/hirsel-client-core/src/client.rs:438-455

~~~rust
/// Register a push token once the WebSocket is online. Registrations made
/// while disconnected remain queued until the next successful handshake.
pub fn register_push_token(&self, platform: String, token: String) -> Result<(), ClientError> {
    let platform = match platform.as_str() {
        "android" => PushPlatform::Android,
        "web" => PushPlatform::Web,
        "ios" => PushPlatform::Ios,
        _ => return Err(ClientError::UnsupportedPushPlatform(platform)),
    };
    if token.trim().is_empty() {
        return Err(ClientError::EmptyPushToken);
    }

    self.inner
        .pending_frames
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push_back(ClientToHost::RegisterPushToken { platform, token });
~~~

crates/hirsel-client-ffi/src/lib.rs:478-482

~~~rust
pub fn register_push_token(&self, platform: String, token: String) -> Result<(), ClientError> {
    self.core
        .register_push_token(platform, token)
        .map_err(Into::into)
}
~~~

The wire command and host table are keyed by the registered token:

crates/hirsel-proto/src/client.rs:182-188

~~~rust
RegisterPushToken {
    platform: PushPlatform,
    token: String,
},
UnregisterPushToken {
    token: String,
},
~~~

crates/hirsel-host/src/storage/current.sql:79-84

~~~sql
CREATE TABLE push_tokens (
    token TEXT PRIMARY KEY,
    platform TEXT NOT NULL,
    created_ts TEXT NOT NULL,
    last_seen_ts TEXT NOT NULL
);
~~~

The host upsert is idempotent for a token, but only runs when the client calls
register:

crates/hirsel-host/src/storage/push_tokens.rs:14-36

~~~rust
pub async fn register_push_token(
    &self,
    platform: PushPlatform,
    token: impl Into<String>,
) -> anyhow::Result<PushToken> {
    let token = token.into();
    if token.trim().is_empty() {
        anyhow::bail!("push token must not be empty");
    }
    let now = Utc::now();
    let conn = self.conn.lock().await;
    conn.execute(
        "
        INSERT INTO push_tokens (token, platform, created_ts, last_seen_ts)
        VALUES (?1, ?2, ?3, ?3)
        ON CONFLICT(token) DO UPDATE SET
            platform = excluded.platform,
            last_seen_ts = excluded.last_seen_ts
        ",
        params![token, push_platform_to_str(platform), now.to_rfc3339()],
    )?;
    get_push_token(&conn, &token).map_err(Into::into)
}
~~~

### Concrete invalid state and reachability

The invalid state is:

~~~text
FCM current token = T2
settings.pushEnabled = true
connection.isOnline = true
host push_tokens = { T1 }
LaunchedEffect key remains true and onNewToken has no registration path
~~~

It is reachable after initial registration of T1 whenever Firebase invokes
onNewToken with T2 without changing connection.isOnline. The refresh callback
does not write T2 to an app-owned current-token state. The effect does not
restart, so the existing registerPushToken call is not made for T2. Host sends
to its old T1 row until the app later reconnects or recreates the activity.

A second reachable version occurs when the root has already reached online, the
SettingsScreen toggles push off/on, and no connection-key change occurs: the
child screen updates its local state and SharedPreferences, but the parent
effect is not keyed by that transition. The service's direct pushEnabled check
still suppresses disabled delivery, so the immediate visible failure is
conditional on the token also needing re-registration.

### Duplicate-truth assessment

There is a concrete distributed duplicate of delivery identity:

1. MainActivity sends T1 through registerPushToken at MainActivity:230-233;
   the host stores T1 in push_tokens.
2. Firebase owns the current T2 and invokes onNewToken.
3. The C22 callback only logs T2 at HirselFirebaseMessagingService:20-22;
   it does not update the host row or any durable C22 current-token state.

Thus one delivery identity changes while the other remains stale. No separate
Android local current-token copy exists today, which is itself the missing
representation. The existing host table is the delivery set and should remain
the host-side authority; adding another host “current token” column would
amplify rather than solve the problem.

### Reproducible consumer query

From the repository root:

~~~bash
rg -n 'onNewToken|fetchFcmToken|registerPushToken|pushEnabled|connection\.isOnline' android/app/src/main/kotlin/dev/hirsel/android crates/hirsel-client-ffi/src/lib.rs crates/hirsel-client-core/src/client.rs
~~~

This returned 20 matching lines in the audited snapshot. The result covers the
refresh callback, token fetch, online-gated registration, push preference, and
FFI/core registration path.

### Exact target representation

Introduce one app-scoped registration state, without adding a second host
token representation:

~~~kotlin
data class PushRegistrationState(
    val enabled: Boolean,
    val currentToken: String?,
)
~~~

The target behavior at each layer is:

- android/app/src/main/kotlin/dev/hirsel/android/PushRegistration.kt owns the
  app-scoped state: persist the latest currentToken in app-private storage and
  expose PushRegistrationState as a StateFlow. Keep enabled sourced from the
  existing SettingsStore.pushEnabled; do not create a second durable
  preference for the same boolean.
- HirselFirebaseMessagingService: onNewToken publishes the new token to that
  app-scoped state. It does not attempt to use an Activity connection.
- HirselRoot/MainActivity: collect the state and key the registration effect on
  connection.isOnline, enabled, and currentToken. When online, enabled, and
  currentToken is non-null, call the existing registerPushToken with the
  current token. An online token change must issue a new idempotent
  registration.
- SettingsScreen: emit the existing push-enabled change to the same
  app-scoped state/coordinator while SettingsStore persists the boolean. The
  root must observe the event rather than only reading a non-reactive
  SharedPreferences property during composition.
- FFI/core/protocol/host: retain RegisterPushToken { platform, token } and
  the existing push_tokens(token PRIMARY KEY, platform, timestamps) table. No
  new current-token column or push-scope field is required for refresh repair.
  The existing UnregisterPushToken protocol is outside this minimal repair.

This removes the invalid state in which the app knows a current token but has
no state transition capable of registering it. It also keeps one durable
Android preference for enabled and one host delivery-set authority instead of
adding parallel current-token copies.

### Smallest credible affected files and ownership

- android/app/src/main/kotlin/dev/hirsel/android/HirselFirebaseMessagingService.kt:
  publish refreshes.
- android/app/src/main/kotlin/dev/hirsel/android/MainActivity.kt:
  collect state and key the effect on all three inputs.
- android/app/src/main/kotlin/dev/hirsel/android/SettingsScreen.kt:
  route the push toggle through the observed registration state.
- android/app/src/main/kotlin/dev/hirsel/android/PushRegistration.kt:
  new single owner for PushRegistrationState, including current-token
  persistence and its StateFlow.
- android/app/src/main/kotlin/dev/hirsel/android/settings/SettingsStore.kt:
  keep the existing single durable enabled preference; it does not gain a
  second token or scope representation.
- Existing FFI/core/protocol interfaces are sufficient for refresh registration.
  No generated binding edit or host DDL change is required.

### Regression and cutover risk

The registration call is already queued by the client while disconnected, so
keying it on currentToken should preserve offline pairing behavior. The main
risks are duplicate registrations during recomposition and retaining old host
tokens. Use a last-attempt/current-token guard in the coordinator and keep the
existing host upsert semantics. Stale-token cleanup remains outside this
minimal repair.

Do not make the Firebase service depend directly on the Activity or a live
Connection. That would recreate the lifecycle coupling this finding identifies.

### Existing and additional validation

The existing client-core test demonstrates that an explicit registration queues
until the client is online:

crates/hirsel-client-core/tests/client_flow.rs:449-478

~~~rust
#[tokio::test]
async fn push_token_registration_queues_until_the_client_is_online() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let _hello = receive_client(&mut socket).await;
        send_hello(&mut socket, vec![], vec![], vec![]).await;
        receive_client(&mut socket).await
    });

    let client = Client::new(test_config(address)).unwrap();
    client
        .register_push_token("android".into(), "fcm-token".into())
        .unwrap();
    client.connect().await.unwrap();

    assert_eq!(
        timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap(),
        ClientToHost::RegisterPushToken {
            platform: hirsel_proto::PushPlatform::Android,
            token: "fcm-token".into(),
        },
    );
    client.disconnect().await;
}
~~~

That fixture proves the transport command, not the Android refresh callback,
preference transition, or current-token synchronization. The protocol tests
also round-trip RegisterPushToken and UnregisterPushToken, but no owned Android
test demonstrates onNewToken repair. No tests were executed in this audit.

Additional validation required later, but not run here:

- Unit-test the coordinator with T1 then T2 while online and assert exactly one
  registration for each current token, including a refresh delivered while
  the Activity is absent.
- Assert a reconnect seeds/registers the current persisted token and that
  repeated Compose recomposition does not issue uncontrolled duplicate
  registrations.
- Assert push disable/enable updates the observed state and retains the
  service's local suppression behavior.
- Test the real FFI registration path with a fake transport.

Confidence: high.

## Explicit no-finding inventory and skips

All 31 assigned paths were inspected. Files involved in F1/F2 are listed with
the finding number; all other files have an explicit no-finding or skip
reason. Shared named definitions were not claimed as C22-owned.

| Assigned path | Result |
|---|---|
| android/app/build.gradle.kts | No finding. Native dependencies and Android build wiring were inspected; no confirmed representation/control-flow defect. |
| android/app/src/main/AndroidManifest.xml | No finding. Activity, service, permissions, and application declarations were inspected; no confirmed manifest defect. |
| android/app/src/main/kotlin/dev/hirsel/android/Components.kt | No finding. Shared Compose controls use explicit semantics/sizing and were not a source of a material defect. |
| android/app/src/main/kotlin/dev/hirsel/android/HirselFirebaseMessagingService.kt | F1 consumer contract and F2 token refresh callback. |
| android/app/src/main/kotlin/dev/hirsel/android/MainActivity.kt | F1 notification intent/snapshot route and F2 one-shot registration effect. |
| android/app/src/main/kotlin/dev/hirsel/android/NotificationDestination.kt | No independent finding. History-aware rejection is the intentional safety invariant used by F1. |
| android/app/src/main/kotlin/dev/hirsel/android/SettingsScreen.kt | F2 preference observation boundary. NotifyScope was inspected and skipped as explicit intent-only behavior, not counted as a third finding. |
| android/app/src/main/kotlin/dev/hirsel/android/chat/ChatScreen.kt | No finding. Snapshot rendering, independent Thread dimensions, message entry, and current-history use were inspected. Artifact bytes/viewer gap is tracked/planned #10 and is not re-reported. |
| android/app/src/main/kotlin/dev/hirsel/android/chat/ThreadInstrument.kt | No finding. Instrument selector/JSON display behavior was inspected; no materially invalid state was established. |
| android/app/src/main/kotlin/dev/hirsel/android/chat/ThreadNavigation.kt | No finding. Nested forest ordering, filtered parents, zero IDs, and cycle bounding are explicit and covered by tests. |
| android/app/src/main/kotlin/dev/hirsel/android/onboarding/QrScanner.kt | No finding. Camera lifecycle, single-result gating, and proxy closure were inspected. |
| android/app/src/main/kotlin/dev/hirsel/android/pairing/Connection.kt | No new finding. History-aware related/retry paths are explicit. Missing history on older mutation commands is the excluded tracked F02/#19 outcome, not re-reported. |
| android/app/src/main/kotlin/dev/hirsel/android/pairing/PairingLink.kt | No finding. Scheme/host/query parsing is strict and bounded. |
| android/app/src/main/kotlin/dev/hirsel/android/pairing/RelatedNavigation.kt | No finding. Related Thread navigation reuses the history and availability guard. |
| android/app/src/main/kotlin/dev/hirsel/android/pairing/TokenStore.kt | No finding. Encrypted secret storage, separate from plain settings, was inspected. |
| android/app/src/main/kotlin/dev/hirsel/android/settings/SettingsStore.kt | F2 retains pushEnabled as the single durable preference. NotifyScope is intentionally not counted: current host delivery is response-only and the historical source comment called the scope intent-only. |
| android/app/src/main/kotlin/dev/hirsel/android/ui/ErrorCopy.kt | No finding. Error classification/copy paths were inspected; no representational defect. |
| android/app/src/main/kotlin/dev/hirsel/android/ui/Theme.kt | No finding. Light/dark semantic color tokens and Material mappings were inspected; the static brand palette is deliberate. |
| android/app/src/main/kotlin/dev/hirsel/core/hirsel_client_ffi.kt | No finding. Generated output was inspected as a consumer of the Rust generation pipeline; no mismatch was found. It is not directly edited. |
| android/app/src/main/res/values/strings.xml | No finding. Resource strings were inspected; no material missing/duplicated resource state. |
| android/app/src/main/res/values/styles.xml | No finding. Native theme/style bootstrap was inspected. |
| android/app/src/main/res/xml/network_security_config.xml | No finding. Network security declarations were inspected; no confirmed invalid configuration. |
| crates/hirsel-client-ffi/src/bin/uniffi-bindgen.rs | No finding. Binding-generation entrypoint was inspected; no generation ownership/control defect. |
| crates/hirsel-client-ffi/src/lib.rs | No finding. Rust FFI enums, nullable fields, records, command methods, callbacks, and error conversion were inspected. Push registration is a separate interface referenced by F2. |
| crates/hirsel-client-ffi/src/threads.rs | No finding. Thread/Turn/Activity/Stream conversion and explicit state serialization were inspected; no invalid representable conversion was established. |
| crates/hirsel-client-ffi/uniffi.toml | No finding. UniFFI generation configuration was inspected. |
| android/.maestro/smoke.yaml | No finding. Smoke flow coverage was inspected; no source representation defect. |
| android/app/src/main/kotlin/dev/hirsel/android/chat/ThreadAvatar.kt | No finding. Icon fallback, Unicode bounds, and decorative semantics are intentional and tested. |
| android/app/src/test/kotlin/dev/hirsel/android/NotificationDestinationTest.kt | Positive coverage for history matching, missing history, stale IDs, and ordinary zero; it does not cover FCM wire serialization. |
| android/app/src/test/kotlin/dev/hirsel/android/chat/ThreadNavigationTest.kt | Positive coverage for nested ordering, filtered parents, pins, zero, and malformed cycles; no new finding. |
| android/app/src/test/kotlin/dev/hirsel/android/pairing/RelatedItemsBindingTest.kt | Positive coverage for generated RelatedItem/ClientSnapshot/LifecycleEvent round trips and history-aware related navigation; no new finding. |

### Explicit exclusion handling

- The exclusions file was read before reporting.
- Existing tracked #2-14 outcomes were not reported as new findings.
- Native artifact viewing/downloading is the explicitly planned #10 gap and was
  skipped.
- Missing history on the older identity-bound mutation commands is the
  excluded F02/#19 outcome and was skipped.
- Product Thread dimensions such as pinning, attention, read state, execution,
  settlement, and visibility were preserved as independent dimensions.
- Generated Kotlin was reviewed as generated output; the Rust FFI definition
  and generation seam remains the implementation owner.

## Patterns and systemic observations

1. The destination guard is stronger than the wire-contract test surface. The
   Android side correctly rejects unsafe partial identity, but the producer's
   final JSON projection is not checked against that invariant.
2. Lifecycle side effects are attached to a single connection transition rather
   than an explicit state containing current token and desired preference. This
   makes refresh and preference transitions invisible without requiring any
   additional host schema.
3. The FFI model itself is not the source of the push defect. The generated
   record converters and native Thread representations preserve explicit
   nullable/enum/list shapes, and existing tests cover important history-aware
   binding paths.

## Positive findings

- NotificationDestination treats numeric Thread IDs as meaningful only in
  their issuing history and retains ID 0 as an ordinary root.
- Related-item and snapshot tests verify nullable titles, URLs, history changes,
  empty reset lists, callback events, and no remaining buffer bytes.
- ThreadNavigation bounds malformed parent cycles without dropping reachable
  rows and keeps independent pin/read/attention behavior out of one overloaded
  flag.
- TokenStore separates encrypted identity/device secrets from plain settings.
- PairingLink rejects incorrect scheme/host/query shapes, and QrScanner closes
  camera proxy resources after processing.
- Compose text uses sp, controls carry semantic/test metadata, and the app
  applies edge-to-edge plus status/navigation/IME padding.

## Recommended actions

These are implementation follow-up commands only; no command was run as part
of this read-only audit.

1. [P1] $impeccable harden: repair the host FCM JSON projection and add a
   cross-layer fixture asserting history_id, thread_id, and title survive into
   the Android notification intent.
2. [P2] $impeccable harden: introduce the app-scoped PushRegistrationState
   and make token refresh, preference changes, and online transitions converge
   on one idempotent registration path.
3. $impeccable polish: perform the final Android quality pass after both
   contract/lifecycle fixes and their validation are integrated.

You can ask me to run these one at a time, all at once, or in any order you
prefer.

Re-run $impeccable audit after fixes to see your score improve.

## Final source verification

The post-report repository checks were:

~~~text
git rev-parse HEAD HEAD^{tree}
3ee0621a603659ab0168f565b99012b642415419
a4aac830c45398a66591f2c44b707aaf3cef281b

git status --porcelain
[empty]
~~~

The only created deliverable is this report at
/tmp/hirsel-combined-audit/workers/C22-FFI-ANDROID.md. Source is unchanged.
