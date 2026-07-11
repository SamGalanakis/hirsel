package dev.hirsel.android

import android.Manifest
import android.app.Activity
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.ui.platform.LocalView
import androidx.core.view.WindowCompat
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
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
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import dev.hirsel.android.chat.ChatScreen
import dev.hirsel.android.onboarding.QrScanner
import dev.hirsel.android.pairing.Connection
import dev.hirsel.android.pairing.ConnectionSpec
import dev.hirsel.android.pairing.DeviceCredential
import dev.hirsel.android.pairing.Phase
import dev.hirsel.android.pairing.PairingLinkResult
import dev.hirsel.android.pairing.TokenStore
import dev.hirsel.android.pairing.parsePairingLink
import dev.hirsel.android.pairing.rememberConnection
import dev.hirsel.android.settings.SettingsStore
import dev.hirsel.android.ui.ErrorCopy
import dev.hirsel.android.ui.LocalHirselColors
import dev.hirsel.android.ui.ThemeMode
import dev.hirsel.android.ui.HirselTheme
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
        // Edge-to-edge with transparent system bars; the icon appearance is driven
        // reactively from the active theme below so light mode gets dark icons.
        enableEdgeToEdge()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        }
        val settings = SettingsStore(this)
        setContent {
            // Read synchronously from prefs so the chosen scheme is set before the
            // first paint — no light/dark flash on cold start.
            var themeMode by remember { mutableStateOf(settings.themeMode) }
            // Status/nav-bar icons must contrast the theme canvas: dark glyphs on
            // the light scheme, light glyphs on dark. Recomputed on theme change.
            val dark = when (themeMode) {
                ThemeMode.SYSTEM -> isSystemInDarkTheme()
                ThemeMode.LIGHT -> false
                ThemeMode.DARK -> true
            }
            val view = LocalView.current
            LaunchedEffect(dark) {
                val window = (view.context as Activity).window
                WindowCompat.getInsetsController(window, view).apply {
                    isAppearanceLightStatusBars = !dark
                    isAppearanceLightNavigationBars = !dark
                }
            }
            HirselTheme(themeMode) {
                Surface(
                    modifier = Modifier
                        .fillMaxSize()
                        .semantics { testTagsAsResourceId = true },
                    color = LocalHirselColors.current.Background,
                ) {
                    HirselRoot(
                        settings = settings,
                        themeMode = themeMode,
                        onThemeModeChange = { themeMode = it; settings.themeMode = it },
                    )
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
private fun HirselRoot(
    settings: SettingsStore,
    themeMode: ThemeMode,
    onThemeModeChange: (ThemeMode) -> Unit,
) {
    val context = LocalContext.current
    val store = remember { TokenStore(context) }
    var credential by remember { mutableStateOf(store.load()) }
    var pairingSpec by remember { mutableStateOf<ConnectionSpec.Pairing?>(null) }
    var showSettings by remember { mutableStateOf(false) }
    // "Pair a new device" re-enters the scan flow even though a device is already
    // paired; a successful pairing replaces this device's credential.
    var addingDevice by remember { mutableStateOf(false) }

    // A live pairing session wins over any stored credential so the freshly
    // authenticated connection carries straight through into chat.
    val activeSpec: ConnectionSpec? = pairingSpec
        ?: credential?.let { ConnectionSpec.Device(it) }

    if (activeSpec == null || addingDevice) {
        PairEntry(
            onSubmit = { pairingSpec = it; addingDevice = false },
            onBack = if (addingDevice) ({ addingDevice = false }) else null,
        )
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

    // Best-effort FCM registration once the transport is up (Ping push tokens),
    // gated on the user's push preference.
    LaunchedEffect(connection.isOnline) {
        if (!connection.isOnline || !settings.pushEnabled) return@LaunchedEffect
        runCatching {
            val token = fetchFcmToken()
            Log.i(FCM_LOG_TAG, "FCM token fetched: ${token.take(16)}…")
            withContext(Dispatchers.IO) { connection.client?.registerPushToken("android", token) }
            Log.i(FCM_LOG_TAG, "FCM token registered with Hirsel host")
        }.onFailure { Log.e(FCM_LOG_TAG, "FCM token registration failed", it) }
    }

    val forget = {
        store.clear()
        credential = null
        pairingSpec = null
        showSettings = false
    }

    val activeLabel = credential?.deviceLabel
        ?: (activeSpec as? ConnectionSpec.Pairing)?.label.orEmpty()

    when {
        // While a fresh pairing is still handshaking (and no token yet), show progress.
        activeSpec is ConnectionSpec.Pairing && credential == null ->
            PairingProgress(phase = connection.phase, label = activeSpec.label, onCancel = { pairingSpec = null })

        showSettings ->
            SettingsScreen(
                themeMode = themeMode,
                onThemeModeChange = onThemeModeChange,
                settings = settings,
                phase = connection.phase,
                deviceLabel = activeLabel,
                identitySecret = credential?.irohSecretKey,
                appVersion = appVersionLabel(context),
                onBack = { showSettings = false },
                onRename = { newLabel ->
                    credential?.let { current ->
                        val updated = current.copy(deviceLabel = newLabel)
                        store.save(updated)
                        credential = updated
                    }
                },
                onForget = forget,
                onResetIdentity = forget,
                onPairNew = { showSettings = false; addingDevice = true },
            )

        else ->
            ChatScreen(
                connection = connection,
                onOpenSettings = { showSettings = true },
            )
    }
}

/** Human app version + build, e.g. "0.1 (1)", read from the package manager. */
private fun appVersionLabel(context: android.content.Context): String = runCatching {
    val pkg = context.packageManager.getPackageInfo(context.packageName, 0)
    val code = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) pkg.longVersionCode else pkg.versionCode.toLong()
    "${pkg.versionName} ($code)"
}.getOrDefault("unknown")

// ---------------------------------------------------------------------------
// Onboarding
// ---------------------------------------------------------------------------

private fun defaultDeviceLabel(): String {
    val model = Build.MODEL?.trim().orEmpty()
    return model.ifEmpty { "${Build.MANUFACTURER} phone" }
}

/** Max characters for a device label — the pairing label the host stores. */
internal const val MAX_DEVICE_LABEL = 40

/** Guards the device-name input: strips control/newline chars and caps length (D12). */
internal fun sanitizeDeviceLabel(raw: String): String =
    raw.replace(Regex("""[\r\n\t]"""), " ").take(MAX_DEVICE_LABEL)

@Composable
private fun PairEntry(onSubmit: (ConnectionSpec.Pairing) -> Unit, onBack: (() -> Unit)? = null) {
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
        Spacer(Modifier.height(8.dp))
        if (onBack != null) {
            TextButton(
                onClick = onBack,
                contentPadding = androidx.compose.foundation.layout.PaddingValues(0.dp),
                modifier = Modifier.testTag("pair-back"),
            ) {
                Text("‹  Back", color = LocalHirselColors.current.MutedForeground, fontSize = 13.sp)
            }
            Spacer(Modifier.height(6.dp))
        }
        Text(
            "hirsel",
            style = microLabel(),
            color = LocalHirselColors.current.MutedForeground,
        )
        Spacer(Modifier.height(10.dp))
        Text(
            if (onBack != null) "Pair a new device" else "Pair with your host",
            style = androidx.compose.material3.MaterialTheme.typography.titleLarge,
            color = LocalHirselColors.current.Foreground,
        )
        Spacer(Modifier.height(6.dp))
        Text(
            "Point the camera at the pairing QR on your host, or paste its link.",
            fontSize = 13.sp,
            lineHeight = 19.sp,
            color = LocalHirselColors.current.MutedForeground,
        )
        Spacer(Modifier.height(20.dp))

        // Scanner viewport — signature affordance of the scan-first flow.
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .aspectRatio(1f)
                .clip(RoundedCornerShape(14.dp))
                .background(LocalHirselColors.current.Card, RoundedCornerShape(14.dp))
                .border(1.dp, LocalHirselColors.current.Border, RoundedCornerShape(14.dp))
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
                        color = LocalHirselColors.current.Foreground,
                        fontWeight = FontWeight.SemiBold,
                        fontSize = 14.sp,
                    )
                    Spacer(Modifier.height(4.dp))
                    Text(
                        "Grant camera access to scan, or paste the link below.",
                        color = LocalHirselColors.current.MutedForeground,
                        fontSize = 12.sp,
                    )
                    Spacer(Modifier.height(12.dp))
                    TextButton(onClick = { permissionLauncher.launch(Manifest.permission.CAMERA) }) {
                        Text("Enable camera", color = LocalHirselColors.current.AccentRing)
                    }
                }
                else -> CircularProgressIndicator(color = LocalHirselColors.current.AccentRing, modifier = Modifier.size(28.dp))
            }
        }

        Spacer(Modifier.height(22.dp))
        HairlineDivider(label = "or paste a link")
        Spacer(Modifier.height(16.dp))

        Text("This device", style = microLabel(), color = LocalHirselColors.current.MutedForeground)
        Spacer(Modifier.height(6.dp))
        HirselField(
            value = label,
            onValueChange = { label = sanitizeDeviceLabel(it) },
            placeholder = "Device name",
            testTag = "device-label-field",
            singleLine = true,
        )

        Spacer(Modifier.height(14.dp))
        Text("Pairing link", style = microLabel(), color = LocalHirselColors.current.MutedForeground)
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
            isError = error != null,
            imeAction = ImeAction.Go,
            onImeAction = { submit(pasted) },
        )

        if (error != null) {
            Spacer(Modifier.height(8.dp))
            Text(
                error!!,
                color = LocalHirselColors.current.StatusDanger,
                fontSize = 12.sp,
                modifier = Modifier.testTag("link-error"),
            )
        }

        Spacer(Modifier.height(16.dp))
        Button(
            onClick = { submit(pasted) },
            enabled = pasted.isNotBlank(),
            colors = ButtonDefaults.buttonColors(
                containerColor = LocalHirselColors.current.Accent,
                contentColor = LocalHirselColors.current.OnAccent,
                disabledContainerColor = LocalHirselColors.current.Secondary,
                disabledContentColor = LocalHirselColors.current.MutedForeground,
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
    val stroke = LocalHirselColors.current.Foreground.copy(alpha = 0.85f)
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
                StatusDot(LocalHirselColors.current.StatusDanger)
                Spacer(Modifier.height(14.dp))
                Text("Pairing failed", fontWeight = FontWeight.SemiBold, fontSize = 15.sp, color = LocalHirselColors.current.Foreground)
                Spacer(Modifier.height(8.dp))
                Text(
                    friendlyPairError(detail),
                    color = LocalHirselColors.current.MutedForeground,
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
                    colors = ButtonDefaults.buttonColors(containerColor = LocalHirselColors.current.Accent, contentColor = LocalHirselColors.current.OnAccent),
                    shape = RoundedCornerShape(8.dp),
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 24.dp),
                    modifier = Modifier.height(48.dp).testTag("pair-retry"),
                ) { Text("Try another link", fontWeight = FontWeight.SemiBold) }
            }
            else -> {
                CircularProgressIndicator(color = LocalHirselColors.current.AccentRing, modifier = Modifier.size(30.dp))
                Spacer(Modifier.height(18.dp))
                val text = when (phase) {
                    is Phase.Reconnecting -> "Reconnecting to your host…"
                    else -> "Reaching your host over iroh…"
                }
                Text(
                    text,
                    color = LocalHirselColors.current.MutedForeground,
                    fontSize = 13.sp,
                    textAlign = TextAlign.Center,
                    modifier = Modifier
                        .widthIn(max = 300.dp)
                        .testTag("pair-status"),
                )
                Spacer(Modifier.height(6.dp))
                Text(
                    label,
                    fontSize = 12.sp,
                    color = LocalHirselColors.current.MutedForeground.copy(alpha = 0.55f),
                )
                Spacer(Modifier.height(28.dp))
                TextButton(onClick = onCancel, modifier = Modifier.testTag("pair-cancel")) {
                    Text("Cancel", color = LocalHirselColors.current.MutedForeground, fontSize = 13.sp)
                }
            }
        }
    }
}

private fun friendlyPairError(detail: String): String = ErrorCopy.pairing(detail)

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
