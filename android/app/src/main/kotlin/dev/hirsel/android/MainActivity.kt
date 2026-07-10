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
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.input.ImeAction
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
import dev.hirsel.core.ChatAuthor
import dev.hirsel.core.ChatMessage
import dev.hirsel.core.ClientSnapshot
import dev.hirsel.core.Ping
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
        setContent {
            HirselTheme {
                Surface(
                    modifier = Modifier
                        .fillMaxSize()
                        .semantics { testTagsAsResourceId = true },
                    color = Hirsel.Background,
                ) {
                    HirselRoot()
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
        PairingProgress(connection = connection, label = activeSpec.label, onCancel = { pairingSpec = null })
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
private fun PairEntry(onSubmit: (ConnectionSpec.Pairing) -> Unit) {
    val context = LocalContext.current
    var label by remember { mutableStateOf(defaultDeviceLabel()) }
    var pasted by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }

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
        Text("hirsel", style = androidx.compose.material3.MaterialTheme.typography.titleLarge, color = Hirsel.Foreground)
        Spacer(Modifier.height(6.dp))
        Text(
            "Pair with your host",
            fontSize = 15.sp,
            fontWeight = FontWeight.SemiBold,
            color = Hirsel.Foreground,
        )
        Spacer(Modifier.height(4.dp))
        Text(
            "Point the camera at the pairing QR on your host, or paste its link.",
            fontSize = 13.sp,
            color = Hirsel.MutedForeground,
        )
        Spacer(Modifier.height(18.dp))

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
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(40.dp),
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .border(2.dp, Hirsel.Foreground.copy(alpha = 0.65f), RoundedCornerShape(12.dp)),
        )
    }
}

@Composable
private fun PairingProgress(
    connection: Connection,
    label: String,
    onCancel: () -> Unit,
) {
    val phase = connection.phase

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
                Spacer(Modifier.height(6.dp))
                Text(
                    friendlyPairError(detail),
                    color = Hirsel.MutedForeground,
                    fontSize = 13.sp,
                    modifier = Modifier.testTag("pair-status"),
                )
                Spacer(Modifier.height(22.dp))
                Button(
                    onClick = onCancel,
                    colors = ButtonDefaults.buttonColors(containerColor = Hirsel.Accent, contentColor = Hirsel.Foreground),
                    shape = RoundedCornerShape(8.dp),
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
                    modifier = Modifier.testTag("pair-status"),
                )
                Spacer(Modifier.height(8.dp))
                Text(label, style = microLabel(), color = Hirsel.MutedForeground)
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
            Spacer(Modifier.height(8.dp))
            UnpairConfirm(label = label, onConfirm = onUnpair, onDismiss = { confirmUnpair = false })
        }

        Spacer(Modifier.height(10.dp))
        HairlineDivider()
        Spacer(Modifier.height(10.dp))

        LazyColumn(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .testTag("chat-list"),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (snapshot == null) {
                item { EmptyLine("Connecting over iroh…") }
            } else if (snapshot.messages.isEmpty()) {
                item { EmptyLine("No messages yet.") }
            } else {
                items(
                    snapshot.messages,
                    key = { "message-${it.id?.toString() ?: it.clientId.orEmpty()}" },
                ) { MessageRow(it) }
            }

            if (pings.isNotEmpty()) {
                item {
                    Spacer(Modifier.height(4.dp))
                    Text("Pings", style = microLabel(), color = Hirsel.MutedForeground)
                }
                items(pings, key = { "ping-${it.id}" }) { PingCard(it) }
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
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .border(1.dp, Hirsel.Border, RoundedCornerShape(10.dp))
            .background(Hirsel.Card, RoundedCornerShape(10.dp))
            .padding(14.dp),
    ) {
        Text("Forget this device?", fontWeight = FontWeight.SemiBold, color = Hirsel.Foreground, fontSize = 14.sp)
        Spacer(Modifier.height(4.dp))
        Text(
            "Clears the stored token for “$label”. You'll need a new pairing link to reconnect.",
            color = Hirsel.MutedForeground,
            fontSize = 12.sp,
        )
        Spacer(Modifier.height(10.dp))
        Row(horizontalArrangement = Arrangement.End, modifier = Modifier.fillMaxWidth()) {
            TextButton(onClick = onDismiss) { Text("Cancel", color = Hirsel.MutedForeground) }
            Spacer(Modifier.width(6.dp))
            TextButton(onClick = onConfirm, modifier = Modifier.testTag("unpair-confirm")) {
                Text("Forget", color = Hirsel.StatusDanger, fontWeight = FontWeight.SemiBold)
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
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = if (owner) Arrangement.End else Arrangement.Start,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth(0.82f)
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
                overflow = TextOverflow.Ellipsis,
            )
            if (owner && message.pending) {
                Spacer(Modifier.height(2.dp))
                Text("sending…", color = Hirsel.Foreground.copy(alpha = 0.7f), fontSize = 11.sp)
            }
        }
    }
}

@Composable
private fun PingCard(ping: Ping) {
    val requires = ping.requiresResponse
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(Hirsel.Card, RoundedCornerShape(14.dp))
            .border(1.dp, Hirsel.Border, RoundedCornerShape(14.dp)),
    ) {
        // Requires-response Pings carry a persistent indigo left stripe.
        Box(
            modifier = Modifier
                .width(3.dp)
                .height(48.dp)
                .background(if (requires) Hirsel.Accent else androidx.compose.ui.graphics.Color.Transparent),
        )
        Column(Modifier.padding(horizontal = 12.dp, vertical = 10.dp)) {
            Text(
                "@${ping.name}",
                color = Hirsel.Foreground,
                fontWeight = FontWeight.SemiBold,
                fontSize = 13.sp,
                fontFamily = HirselMono,
                modifier = Modifier.testTag("ping-name"),
            )
            Spacer(Modifier.height(2.dp))
            Text(ping.description, color = Hirsel.MutedForeground, fontSize = 13.sp)
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
private fun EmptyLine(text: String) {
    Text(text, color = Hirsel.MutedForeground, fontSize = 13.sp)
}

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
