package dev.hirsel.android

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import dev.hirsel.android.onboarding.QrScanner
import dev.hirsel.android.pairing.Connection
import dev.hirsel.android.pairing.ConnectionSpec
import dev.hirsel.android.pairing.DeviceCredential
import dev.hirsel.android.pairing.Phase
import dev.hirsel.android.pairing.PairingLinkResult
import dev.hirsel.android.pairing.TokenStore
import dev.hirsel.android.pairing.parsePairingLink
import dev.hirsel.android.pairing.rememberConnection
import dev.hirsel.android.ui.Hirsel
import dev.hirsel.android.ui.HirselMono
import dev.hirsel.android.ui.HirselTheme
import dev.hirsel.core.AgentActivity
import dev.hirsel.core.AgentActivityState
import dev.hirsel.core.ChatAuthor
import dev.hirsel.core.ChatMessage
import dev.hirsel.core.ClientSnapshot
import dev.hirsel.core.ConnectionState
import dev.hirsel.core.Ping
import dev.hirsel.core.PingStatus
import dev.hirsel.core.generateIrohIdentity
import com.google.firebase.messaging.FirebaseMessaging
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlin.coroutines.suspendCoroutine
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        }
        val previewScreen = intent?.getStringExtra("preview")
        setContent {
            HirselTheme {
                Surface(
                    modifier = Modifier
                        .fillMaxSize()
                        .semantics { testTagsAsResourceId = true },
                    color = Hirsel.Background,
                ) {
                    if (previewScreen != null) DesignPreview(previewScreen) else HirselRoot()
                }
            }
        }
    }
}

/**
 * Onboarding vs. chat is driven by whether a device token is persisted. When a
 * fresh pairing completes we keep the SAME authenticated [Connection] alive for
 * chat rather than swapping to a new one: the device token is pinned to the
 * client's iroh NodeId. The pairing flow generates one persistent iroh identity,
 * passes it to that client, and saves it with the issued token. A cold relaunch
 * takes the `newIroh` device-token path with the same identity and therefore the
 * same pinned NodeId.
 */
@Composable
private fun HirselRoot() {
    val context = LocalContext.current
    val store = remember { TokenStore(context) }
    var credential by remember { mutableStateOf(store.load()) }
    var pairingSpec by remember { mutableStateOf<ConnectionSpec.Pairing?>(null) }

    // A live pairing session wins over any stored credential so the freshly
    // authenticated connection carries straight through into chat.
    val activeSpec: ConnectionSpec? = pairingSpec
        ?: credential?.let { ConnectionSpec.Device(it) }

    if (activeSpec == null) {
        PairEntry(onSubmit = { pairingSpec = it })
        return
    }

    val connection = rememberConnection(activeSpec)

    // On a successful pairing handshake, capture + persist the issued device token.
    if (activeSpec is ConnectionSpec.Pairing) {
        LaunchedEffect(connection.phase) {
            if (connection.phase is Phase.Online && credential == null) {
                var token: String? = null
                repeat(20) {
                    token = connection.issuedDeviceToken()
                    if (token != null) return@repeat
                    delay(100)
                }
                token?.let {
                    val cred = DeviceCredential(
                        ticket = activeSpec.ticket,
                        deviceToken = it,
                        deviceLabel = activeSpec.label,
                        irohSecretKey = activeSpec.irohSecretKey,
                    )
                    store.save(cred)
                    credential = cred
                }
            }
        }
    }

    // Best-effort FCM registration once the transport is up (Ping push tokens).
    LaunchedEffect(connection.isOnline) {
        if (!connection.isOnline) return@LaunchedEffect
        runCatching {
            val token = fetchFcmToken()
            Log.i(FCM_LOG_TAG, "FCM token fetched: ${token.take(16)}…")
            withContext(Dispatchers.IO) { connection.client?.registerPushToken("android", token) }
            Log.i(FCM_LOG_TAG, "FCM token registered with Hirsel host")
        }.onFailure { Log.e(FCM_LOG_TAG, "FCM token registration failed", it) }
    }

    val unpair = {
        store.clear()
        credential = null
        pairingSpec = null
    }

    // While a fresh pairing is still handshaking (and no token yet), show progress;
    // otherwise render chat over the authenticated connection.
    if (activeSpec is ConnectionSpec.Pairing && credential == null) {
        PairingProgress(phase = connection.phase, label = activeSpec.label, onCancel = { pairingSpec = null })
    } else {
        ChatScreen(
            snapshot = connection.snapshot,
            phase = connection.phase,
            label = credential?.deviceLabel ?: (activeSpec as? ConnectionSpec.Pairing)?.label.orEmpty(),
            onSend = connection::send,
            onUnpair = unpair,
        )
    }
}

// ---------------------------------------------------------------------------
// Onboarding
// ---------------------------------------------------------------------------

private fun defaultDeviceLabel(): String {
    val model = Build.MODEL?.trim().orEmpty()
    return model.ifEmpty { "${Build.MANUFACTURER} phone" }
}

@Composable
private fun PairEntry(onSubmit: (ConnectionSpec.Pairing) -> Unit, previewError: String? = null) {
    val context = LocalContext.current
    var label by remember { mutableStateOf(defaultDeviceLabel()) }
    var pasted by remember { mutableStateOf(if (previewError != null) "not-a-pairing-link" else "") }
    var error by remember { mutableStateOf(previewError) }

    var hasCamera by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                PackageManager.PERMISSION_GRANTED,
        )
    }
    var cameraDenied by remember { mutableStateOf(false) }
    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        hasCamera = granted
        cameraDenied = !granted
    }

    LaunchedEffect(Unit) {
        if (!hasCamera) permissionLauncher.launch(Manifest.permission.CAMERA)
    }

    fun submit(raw: String) {
        when (val result = parsePairingLink(raw)) {
            is PairingLinkResult.Ok -> {
                val irohSecretKey = runCatching { generateIrohIdentity() }
                    .getOrElse {
                        error = "Couldn't create a secure device identity. Try again."
                        return
                    }
                error = null
                onSubmit(
                    ConnectionSpec.Pairing(
                        ticket = result.link.ticket,
                        code = result.link.code,
                        label = label.trim().ifEmpty { defaultDeviceLabel() },
                        irohSecretKey = irohSecretKey,
                    ),
                )
            }
            is PairingLinkResult.Invalid -> error = result.reason
        }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .statusBarsPadding()
            .navigationBarsPadding()
            .imePadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 20.dp, vertical = 20.dp),
    ) {
        Spacer(Modifier.height(8.dp))
        Text(
            "hirsel",
            style = microLabel(),
            color = Hirsel.MutedForeground,
        )
        Spacer(Modifier.height(10.dp))
        Text(
            "Pair with your host",
            style = androidx.compose.material3.MaterialTheme.typography.titleLarge,
            color = Hirsel.Foreground,
        )
        Spacer(Modifier.height(6.dp))
        Text(
            "Point the camera at the pairing QR on your host, or paste its link.",
            fontSize = 13.sp,
            lineHeight = 19.sp,
            color = Hirsel.MutedForeground,
        )
        Spacer(Modifier.height(20.dp))

        // Scanner viewport — signature affordance of the scan-first flow.
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .aspectRatio(1f)
                .clip(RoundedCornerShape(14.dp))
                .background(Hirsel.Card, RoundedCornerShape(14.dp))
                .border(1.dp, Hirsel.Border, RoundedCornerShape(14.dp))
                .testTag("scanner-preview"),
            contentAlignment = Alignment.Center,
        ) {
            when {
                hasCamera -> Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(1.dp),
                ) {
                    QrScanner(
                        onQr = { value -> submit(value) },
                        modifier = Modifier.fillMaxSize(),
                    )
                    ScannerReticle()
                }
                cameraDenied -> Column(
                    horizontalAlignment = Alignment.CenterHorizontally,
                    modifier = Modifier.padding(24.dp),
                ) {
                    Text(
                        "Camera off",
                        color = Hirsel.Foreground,
                        fontWeight = FontWeight.SemiBold,
                        fontSize = 14.sp,
                    )
                    Spacer(Modifier.height(4.dp))
                    Text(
                        "Grant camera access to scan, or paste the link below.",
                        color = Hirsel.MutedForeground,
                        fontSize = 12.sp,
                    )
                    Spacer(Modifier.height(12.dp))
                    TextButton(onClick = { permissionLauncher.launch(Manifest.permission.CAMERA) }) {
                        Text("Enable camera", color = Hirsel.AccentRing)
                    }
                }
                else -> CircularProgressIndicator(color = Hirsel.AccentRing, modifier = Modifier.size(28.dp))
            }
        }

        Spacer(Modifier.height(22.dp))
        HairlineDivider(label = "or paste a link")
        Spacer(Modifier.height(16.dp))

        Text("This device", style = microLabel(), color = Hirsel.MutedForeground)
        Spacer(Modifier.height(6.dp))
        HirselField(
            value = label,
            onValueChange = { label = it },
            placeholder = "Device name",
            testTag = "device-label-field",
            singleLine = true,
        )

        Spacer(Modifier.height(14.dp))
        Text("Pairing link", style = microLabel(), color = Hirsel.MutedForeground)
        Spacer(Modifier.height(6.dp))
        HirselField(
            value = pasted,
            onValueChange = {
                pasted = it
                if (error != null) error = null
            },
            placeholder = "hirsel://pair?ticket=…&code=…",
            testTag = "paste-link-field",
            singleLine = false,
            mono = true,
            imeAction = ImeAction.Go,
            onImeAction = { submit(pasted) },
        )

        if (error != null) {
            Spacer(Modifier.height(8.dp))
            Text(
                error!!,
                color = Hirsel.StatusDanger,
                fontSize = 12.sp,
                modifier = Modifier.testTag("link-error"),
            )
        }

        Spacer(Modifier.height(16.dp))
        Button(
            onClick = { submit(pasted) },
            enabled = pasted.isNotBlank(),
            colors = ButtonDefaults.buttonColors(
                containerColor = Hirsel.Accent,
                contentColor = Hirsel.Foreground,
                disabledContainerColor = Hirsel.Secondary,
                disabledContentColor = Hirsel.MutedForeground,
            ),
            shape = RoundedCornerShape(8.dp),
            modifier = Modifier
                .fillMaxWidth()
                .height(48.dp)
                .testTag("pair-button")
                .semantics { contentDescription = "Pair with host" },
        ) {
            Text("Pair", fontWeight = FontWeight.SemiBold)
        }
    }
}

@Composable
private fun ScannerReticle() {
    // Four L-shaped corner brackets read as "scanner" without boxing in the frame.
    val stroke = Hirsel.Foreground.copy(alpha = 0.85f)
    Canvas(
        modifier = Modifier
            .fillMaxSize()
            .padding(44.dp),
    ) {
        val len = size.minDimension * 0.14f
        val w = 2.5.dp.toPx()
        val r = 12.dp.toPx()
        val cap = androidx.compose.ui.graphics.StrokeCap.Round
        fun corner(cx: Float, cy: Float, sx: Int, sy: Int) {
            // Horizontal + vertical arm meeting near the corner, inset by the radius.
            drawLine(stroke, Offset(cx + sx * r, cy), Offset(cx + sx * (r + len), cy), w, cap)
            drawLine(stroke, Offset(cx, cy + sy * r), Offset(cx, cy + sy * (r + len)), w, cap)
        }
        corner(0f, 0f, 1, 1)
        corner(size.width, 0f, -1, 1)
        corner(0f, size.height, 1, -1)
        corner(size.width, size.height, -1, -1)
    }
}

@Composable
private fun PairingProgress(
    phase: Phase,
    label: String,
    onCancel: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .statusBarsPadding()
            .navigationBarsPadding()
            .padding(horizontal = 20.dp, vertical = 20.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        when (phase) {
            is Phase.Failed, is Phase.Offline -> {
                val detail = (phase as? Phase.Failed)?.detail
                    ?: (phase as? Phase.Offline)?.reason
                    ?: "The host didn't answer."
                StatusDot(Hirsel.StatusDanger)
                Spacer(Modifier.height(14.dp))
                Text("Pairing failed", fontWeight = FontWeight.SemiBold, fontSize = 15.sp, color = Hirsel.Foreground)
                Spacer(Modifier.height(8.dp))
                Text(
                    friendlyPairError(detail),
                    color = Hirsel.MutedForeground,
                    fontSize = 13.sp,
                    lineHeight = 20.sp,
                    textAlign = TextAlign.Center,
                    modifier = Modifier
                        .widthIn(max = 300.dp)
                        .testTag("pair-status"),
                )
                Spacer(Modifier.height(24.dp))
                Button(
                    onClick = onCancel,
                    colors = ButtonDefaults.buttonColors(containerColor = Hirsel.Accent, contentColor = Hirsel.Foreground),
                    shape = RoundedCornerShape(8.dp),
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 24.dp),
                    modifier = Modifier.height(48.dp).testTag("pair-retry"),
                ) { Text("Try another link", fontWeight = FontWeight.SemiBold) }
            }
            else -> {
                CircularProgressIndicator(color = Hirsel.AccentRing, modifier = Modifier.size(30.dp))
                Spacer(Modifier.height(18.dp))
                val text = when (phase) {
                    is Phase.Reconnecting -> "Reconnecting to your host…"
                    else -> "Reaching your host over iroh…"
                }
                Text(
                    text,
                    color = Hirsel.MutedForeground,
                    fontSize = 13.sp,
                    textAlign = TextAlign.Center,
                    modifier = Modifier
                        .widthIn(max = 300.dp)
                        .testTag("pair-status"),
                )
                Spacer(Modifier.height(10.dp))
                Text(label, style = microLabel(), color = Hirsel.MutedForeground.copy(alpha = 0.8f))
            }
        }
    }
}

private fun friendlyPairError(detail: String): String = when {
    detail.contains("pairing code", ignoreCase = true) ->
        "This pairing code is expired or already used. Generate a fresh one on your host."
    detail.contains("device_label", ignoreCase = true) ->
        "This device's name doesn't match the pairing code. Re-pair with the name the host expects."
    else -> detail
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

@Composable
private fun ChatScreen(
    snapshot: ClientSnapshot?,
    phase: Phase,
    label: String,
    onSend: (String) -> Unit,
    onUnpair: () -> Unit,
) {
    var draft by remember { mutableStateOf("") }
    var confirmUnpair by remember { mutableStateOf(false) }
    val pings = snapshot?.pings.orEmpty()
    val send = {
        draft.trim().takeIf(String::isNotEmpty)?.let {
            onSend(it)
            draft = ""
        }
        Unit
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .statusBarsPadding()
            .navigationBarsPadding()
            .imePadding()
            .padding(horizontal = 16.dp, vertical = 12.dp),
    ) {
        // Thin top bar: wordmark left, connection pill + overflow right.
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("hirsel", fontSize = 16.sp, fontWeight = FontWeight.SemiBold, color = Hirsel.Foreground)
            Spacer(Modifier.weight(1f))
            ConnectionPill(phase)
            Spacer(Modifier.width(8.dp))
            TextButton(
                onClick = { confirmUnpair = true },
                modifier = Modifier.testTag("unpair"),
            ) { Text("Unpair", color = Hirsel.MutedForeground, fontSize = 13.sp) }
        }

        if (confirmUnpair) {
            UnpairConfirm(label = label, onConfirm = onUnpair, onDismiss = { confirmUnpair = false })
        }

        Spacer(Modifier.height(10.dp))
        HairlineDivider()
        Spacer(Modifier.height(10.dp))

        val messages = snapshot?.messages.orEmpty()
        val hasContent = messages.isNotEmpty() || pings.isNotEmpty()
        Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
            if (!hasContent) {
                val connecting = snapshot == null || phase is Phase.Connecting || phase is Phase.Reconnecting
                ChatPlaceholder(connecting = connecting)
            } else {
                LazyColumn(
                    modifier = Modifier
                        .fillMaxSize()
                        .testTag("chat-list"),
                ) {
                    itemsIndexed(
                        messages,
                        key = { _, m -> "message-${m.id?.toString() ?: m.clientId.orEmpty()}" },
                    ) { index, message ->
                        val prev = messages.getOrNull(index - 1)
                        val gap = when {
                            prev == null -> 0.dp
                            prev.author == message.author -> 3.dp
                            else -> 12.dp
                        }
                        Spacer(Modifier.height(gap))
                        MessageRow(message)
                    }

                    if (pings.isNotEmpty()) {
                        item {
                            Spacer(Modifier.height(if (messages.isEmpty()) 0.dp else 20.dp))
                            Text(
                                "Pings",
                                style = microLabel(),
                                color = Hirsel.MutedForeground,
                                modifier = Modifier.padding(bottom = 8.dp),
                            )
                        }
                        itemsIndexed(pings, key = { _, p -> "ping-${p.id}" }) { index, ping ->
                            if (index > 0) Spacer(Modifier.height(8.dp))
                            PingCard(ping)
                        }
                    }
                    item { Spacer(Modifier.height(4.dp)) }
                }
            }
        }

        Spacer(Modifier.height(10.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(modifier = Modifier.weight(1f)) {
                HirselField(
                    value = draft,
                    onValueChange = { draft = it },
                    placeholder = "Message",
                    testTag = "message-composer",
                    singleLine = true,
                    imeAction = ImeAction.Send,
                    onImeAction = { send() },
                    contentDescription = "Message composer",
                )
            }
            Spacer(Modifier.width(8.dp))
            Button(
                onClick = send,
                enabled = draft.isNotBlank(),
                colors = ButtonDefaults.buttonColors(
                    containerColor = Hirsel.Accent,
                    contentColor = Hirsel.Foreground,
                    disabledContainerColor = Hirsel.Secondary,
                    disabledContentColor = Hirsel.MutedForeground,
                ),
                shape = RoundedCornerShape(8.dp),
                modifier = Modifier
                    .height(48.dp)
                    .testTag("send-message")
                    .semantics { contentDescription = "Send message" },
            ) { Text("Send") }
        }
    }
}

@Composable
private fun UnpairConfirm(label: String, onConfirm: () -> Unit, onDismiss: () -> Unit) {
    // A focused destructive confirm floats above the transcript on a dimmed scrim
    // (the one place a real overlay is warranted).
    Dialog(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .shadow(24.dp, RoundedCornerShape(16.dp), clip = false)
                .clip(RoundedCornerShape(16.dp))
                .background(Hirsel.Card, RoundedCornerShape(16.dp))
                .border(1.dp, Hirsel.Border, RoundedCornerShape(16.dp))
                .padding(20.dp),
        ) {
            Text(
                "Forget this device?",
                fontWeight = FontWeight.SemiBold,
                color = Hirsel.Foreground,
                fontSize = 16.sp,
            )
            Spacer(Modifier.height(8.dp))
            Text(
                "Clears the stored token for “$label”. You'll need a new pairing link to reconnect.",
                color = Hirsel.MutedForeground,
                fontSize = 13.sp,
                lineHeight = 20.sp,
            )
            Spacer(Modifier.height(20.dp))
            Row(horizontalArrangement = Arrangement.End, modifier = Modifier.fillMaxWidth()) {
                TextButton(onClick = onDismiss) { Text("Cancel", color = Hirsel.MutedForeground) }
                Spacer(Modifier.width(6.dp))
                TextButton(onClick = onConfirm, modifier = Modifier.testTag("unpair-confirm")) {
                    Text("Forget", color = Hirsel.StatusDanger, fontWeight = FontWeight.SemiBold)
                }
            }
        }
    }
}

@Composable
private fun ConnectionPill(phase: Phase) {
    val (color, text) = when (phase) {
        is Phase.Online -> Hirsel.StatusSuccess to "online"
        is Phase.Connecting -> Hirsel.StatusAttention to "connecting"
        is Phase.Reconnecting -> Hirsel.StatusAttention to "reconnecting"
        is Phase.Offline -> Hirsel.StatusIdle to "offline"
        is Phase.Failed -> Hirsel.StatusDanger to "error"
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .background(Hirsel.Secondary, RoundedCornerShape(9999.dp))
            .padding(horizontal = 8.dp, vertical = 3.dp)
            .testTag("connection-status")
            .semantics { contentDescription = "Connection $text" },
    ) {
        StatusDot(color, size = 7)
        Spacer(Modifier.width(6.dp))
        Text(text, color = Hirsel.MutedForeground, fontSize = 11.sp, fontWeight = FontWeight.Medium)
    }
}

@Composable
private fun MessageRow(message: ChatMessage) {
    val owner = message.author == ChatAuthor.OWNER
    val time = shortTime(message.timestamp)
    val metaColor = if (owner) Hirsel.Foreground.copy(alpha = 0.62f) else Hirsel.MutedForeground
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = if (owner) Arrangement.End else Arrangement.Start,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth(0.80f)
                .background(
                    if (owner) Hirsel.Accent else Hirsel.Secondary,
                    RoundedCornerShape(14.dp),
                )
                .padding(horizontal = 12.dp, vertical = 8.dp),
        ) {
            Text(
                message.body,
                color = Hirsel.Foreground,
                fontSize = 14.sp,
                lineHeight = 21.sp,
                overflow = TextOverflow.Ellipsis,
            )
            // Meta footer: "sending…" while pending, otherwise a quiet timestamp.
            val footer = when {
                owner && message.pending -> "sending…"
                time != null -> time
                else -> null
            }
            if (footer != null) {
                Spacer(Modifier.height(3.dp))
                Text(
                    footer,
                    color = metaColor,
                    fontSize = 11.sp,
                    modifier = Modifier.align(if (owner) Alignment.End else Alignment.Start),
                )
            }
        }
    }
}

/** Best-effort short HH:mm from an RFC3339/ISO timestamp; falls back to the raw
 *  string when it is already short, or null when there is nothing legible. */
private fun shortTime(raw: String): String? {
    if (raw.isBlank()) return null
    Regex("""T(\d{2}:\d{2})""").find(raw)?.let { return it.groupValues[1] }
    Regex("""^(\d{1,2}:\d{2})""").find(raw)?.let { return it.groupValues[1] }
    return raw.takeIf { it.length <= 8 }
}

@Composable
private fun ChatPlaceholder(connecting: Boolean) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        if (connecting) {
            CircularProgressIndicator(
                color = Hirsel.AccentRing,
                strokeWidth = 2.5.dp,
                modifier = Modifier.size(26.dp),
            )
            Spacer(Modifier.height(16.dp))
            Text("Connecting over iroh…", color = Hirsel.MutedForeground, fontSize = 13.sp)
        } else {
            StatusDot(Hirsel.StatusIdle, size = 8)
            Spacer(Modifier.height(14.dp))
            Text("No messages yet", fontWeight = FontWeight.SemiBold, fontSize = 15.sp, color = Hirsel.Foreground)
            Spacer(Modifier.height(6.dp))
            Text(
                "Send a message to your agent — it's listening.",
                color = Hirsel.MutedForeground,
                fontSize = 13.sp,
                lineHeight = 20.sp,
                textAlign = TextAlign.Center,
                modifier = Modifier.widthIn(max = 280.dp),
            )
        }
    }
}

@Composable
private fun PingCard(ping: Ping) {
    val done = ping.status == PingStatus.DONE
    // Requires-response is the "attend to this" state: persistent indigo stripe,
    // unread dot, and full-strength question text. A Done ping dims and drops it.
    val requires = ping.requiresResponse && !done
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(IntrinsicSize.Min)
            .clip(RoundedCornerShape(14.dp))
            .alpha(if (done) 0.6f else 1f)
            .background(Hirsel.Card, RoundedCornerShape(14.dp))
            .border(1.dp, Hirsel.Border, RoundedCornerShape(14.dp)),
    ) {
        Box(
            modifier = Modifier
                .width(3.dp)
                .fillMaxHeight()
                .background(if (requires) Hirsel.Accent else androidx.compose.ui.graphics.Color.Transparent),
        )
        Column(Modifier.padding(horizontal = 13.dp, vertical = 11.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                if (requires && !ping.read) {
                    StatusDot(Hirsel.Accent, size = 7)
                    Spacer(Modifier.width(7.dp))
                }
                Text(
                    "@${ping.name}",
                    color = Hirsel.Foreground,
                    fontWeight = FontWeight.SemiBold,
                    fontSize = 13.sp,
                    fontFamily = HirselMono,
                    modifier = Modifier.testTag("ping-name"),
                )
                Spacer(Modifier.weight(1f))
                if (done) {
                    Text("✓", color = Hirsel.StatusSuccess, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                    Spacer(Modifier.width(4.dp))
                    Text("Done", style = microLabel(), color = Hirsel.MutedForeground)
                } else {
                    shortTime(ping.timestamp)?.let {
                        Text(it, color = Hirsel.MutedForeground, fontSize = 11.sp)
                    }
                }
            }
            Spacer(Modifier.height(4.dp))
            Text(
                ping.description,
                color = if (requires) Hirsel.Foreground else Hirsel.MutedForeground,
                fontWeight = if (requires) FontWeight.Medium else FontWeight.Normal,
                fontSize = 13.sp,
                lineHeight = 20.sp,
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Shared building blocks
// ---------------------------------------------------------------------------

@Composable
private fun microLabel() = androidx.compose.material3.MaterialTheme.typography.labelMedium.copy(
    letterSpacing = 0.5.sp,
)

@Composable
private fun StatusDot(color: androidx.compose.ui.graphics.Color, size: Int = 10) {
    Box(
        modifier = Modifier
            .size(size.dp)
            .background(color, RoundedCornerShape(9999.dp)),
    )
}

@Composable
private fun HairlineDivider(label: String? = null) {
    if (label == null) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .height(1.dp)
                .background(Hirsel.Border),
        )
    } else {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(modifier = Modifier.weight(1f).height(1.dp).background(Hirsel.Border))
            Text(
                label,
                color = Hirsel.MutedForeground,
                fontSize = 11.sp,
                modifier = Modifier.padding(horizontal = 10.dp),
            )
            Box(modifier = Modifier.weight(1f).height(1.dp).background(Hirsel.Border))
        }
    }
}

@Composable
private fun HirselField(
    value: String,
    onValueChange: (String) -> Unit,
    placeholder: String,
    testTag: String,
    singleLine: Boolean,
    mono: Boolean = false,
    imeAction: ImeAction = ImeAction.Default,
    onImeAction: (() -> Unit)? = null,
    contentDescription: String? = null,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        placeholder = { Text(placeholder, color = Hirsel.MutedForeground.copy(alpha = 0.7f), fontSize = if (mono) 12.sp else 14.sp) },
        singleLine = singleLine,
        maxLines = if (singleLine) 1 else 3,
        textStyle = androidx.compose.material3.MaterialTheme.typography.bodyMedium.copy(
            color = Hirsel.Foreground,
            fontFamily = if (mono) HirselMono else androidx.compose.ui.text.font.FontFamily.Default,
            fontSize = if (mono) 12.sp else 14.sp,
        ),
        keyboardOptions = KeyboardOptions(imeAction = imeAction),
        keyboardActions = KeyboardActions(
            onSend = { onImeAction?.invoke() },
            onGo = { onImeAction?.invoke() },
            onDone = { onImeAction?.invoke() },
        ),
        colors = OutlinedTextFieldDefaults.colors(
            focusedBorderColor = Hirsel.AccentRing,
            unfocusedBorderColor = Hirsel.InputBorder,
            focusedContainerColor = androidx.compose.ui.graphics.Color.Transparent,
            unfocusedContainerColor = androidx.compose.ui.graphics.Color.Transparent,
            cursorColor = Hirsel.AccentRing,
            focusedTextColor = Hirsel.Foreground,
            unfocusedTextColor = Hirsel.Foreground,
        ),
        shape = RoundedCornerShape(8.dp),
        modifier = Modifier
            .fillMaxWidth()
            .testTag(testTag)
            .then(if (contentDescription != null) Modifier.semantics { this.contentDescription = contentDescription } else Modifier),
    )
}

// ---------------------------------------------------------------------------
// Design preview harness (TEMPORARY — removed before final commit). Renders each
// screen/state with deterministic fake data via `am start ... --es preview <name>`.
// ---------------------------------------------------------------------------

private fun fakeMessage(
    id: ULong,
    author: ChatAuthor,
    body: String,
    ts: String,
    pending: Boolean = false,
) = ChatMessage(
    id = id,
    author = author,
    body = body,
    replyTo = null,
    timestamp = ts,
    attachments = emptyList(),
    toolCalls = emptyList(),
    clientId = if (pending) "local-$id" else null,
    pending = pending,
)

private fun fakePing(
    id: ULong,
    name: String,
    description: String,
    requiresResponse: Boolean,
    read: Boolean,
    status: PingStatus,
    ts: String,
) = Ping(
    id = id,
    name = name,
    description = description,
    content = description,
    anchor = 0uL,
    requiresResponse = requiresResponse,
    quickReplies = emptyList(),
    status = status,
    read = read,
    timestamp = ts,
)

@Composable
private fun DesignPreview(name: String) {
    val messages = listOf(
        fakeMessage(1uL, ChatAuthor.OWNER, "Ship it. Ping me only if the tests go red.", "09:12"),
        fakeMessage(2uL, ChatAuthor.AGENT, "Spun up the monitor. I'll ping you if the deploy stalls past 10 minutes.", "09:12"),
        fakeMessage(3uL, ChatAuthor.AGENT, "Migration on stg-a is green; running the smoke suite now.", "09:14"),
        fakeMessage(4uL, ChatAuthor.OWNER, "Which staging DB did it target?", "09:15"),
        fakeMessage(5uL, ChatAuthor.OWNER, "Actually — hold the prod cutover until I'm back.", "09:16", pending = true),
    )
    val pings = listOf(
        fakePing(10uL, "deploy-guard", "Which staging DB should the migration target — stg-a or stg-b?", requiresResponse = true, read = false, status = PingStatus.OPEN, ts = "09:24"),
        fakePing(11uL, "smoke-suite", "Smoke suite passed on stg-a (42/42).", requiresResponse = false, read = true, status = PingStatus.DONE, ts = "09:20"),
    )
    fun snapshot(msgs: List<ChatMessage>, pgs: List<Ping>, state: ConnectionState) = ClientSnapshot(
        connection = state,
        messages = msgs,
        pings = pgs,
        agentActivity = AgentActivity(AgentActivityState.IDLE, null),
        lastSeenMsgId = null,
    )
    when (name) {
        "chat" -> ChatScreen(snapshot(messages, pings, ConnectionState.ONLINE), Phase.Online, "Owner phone", {}, {})
        "chat_empty" -> ChatScreen(snapshot(emptyList(), emptyList(), ConnectionState.ONLINE), Phase.Online, "Owner phone", {}, {})
        "chat_loading" -> ChatScreen(null, Phase.Connecting, "Owner phone", {}, {})
        "chat_unpair" -> ChatScreenWithConfirm(snapshot(messages, pings, ConnectionState.ONLINE), Phase.Online, "Owner phone")
        "pair_connecting" -> PairingProgress(Phase.Connecting, "Owner phone", {})
        "pair_reconnecting" -> PairingProgress(Phase.Reconnecting(2), "Owner phone", {})
        "pair_failed_code" -> PairingProgress(Phase.Failed("host rejected the pairing code"), "Owner phone", {})
        "pair_failed_conn" -> PairingProgress(Phase.Failed("The host didn't answer."), "Owner phone", {})
        "onboarding" -> PairEntry({})
        "onboarding_error" -> PairEntry({}, previewError = "That doesn't look like a pairing link. Paste the full hirsel://pair… URL.")
        else -> ChatScreen(snapshot(messages, pings, ConnectionState.ONLINE), Phase.Online, "Owner phone", {}, {})
    }
}

@Composable
private fun ChatScreenWithConfirm(snapshot: ClientSnapshot, phase: Phase, label: String) {
    // Preview-only: force the unpair confirm open over a populated chat.
    var open by remember { mutableStateOf(true) }
    Box {
        ChatScreen(snapshot, phase, label, {}, {})
        if (open) UnpairConfirm(label = label, onConfirm = { open = false }, onDismiss = { open = false })
    }
}

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
