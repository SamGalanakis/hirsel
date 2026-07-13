package dev.hirsel.android

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.Build
import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.hirsel.android.pairing.Phase
import dev.hirsel.android.settings.NotifyScope
import dev.hirsel.android.settings.SettingsStore
import dev.hirsel.android.ui.HirselMono
import dev.hirsel.android.ui.LocalHirselColors
import dev.hirsel.android.ui.ThemeMode
import java.security.MessageDigest
import java.util.Locale

/**
 * Full-screen Settings (the phone app's second screen), reachable from the chat
 * gear. Grouped calm-terminal sections; every control live-applies and persists.
 *
 * Scope note: the client-core FFI exposes no device roster, NodeId, host version,
 * or log stream, so those surfaces present the honest local truth — this device,
 * a locally-derived identity fingerprint, and a synthesized diagnostics blob —
 * rather than fabricating remote data.
 */
@Composable
fun SettingsScreen(
    themeMode: ThemeMode,
    onThemeModeChange: (ThemeMode) -> Unit,
    settings: SettingsStore,
    phase: Phase,
    deviceLabel: String,
    identitySecret: String?,
    appVersion: String,
    hostVersion: String?,
    onBack: () -> Unit,
    onRename: (String) -> Unit,
    onForget: () -> Unit,
    onResetIdentity: () -> Unit,
    onPairNew: () -> Unit,
) {
    val c = LocalHirselColors.current
    val context = LocalContext.current

    var pushEnabled by remember { mutableStateOf(settings.pushEnabled) }
    var notifyScope by remember { mutableStateOf(settings.notifyScope) }
    var debugMode by remember { mutableStateOf(settings.debugMode) }

    var labelDraft by remember { mutableStateOf(deviceLabel) }
    var confirmForget by remember { mutableStateOf(false) }
    var confirmReset by remember { mutableStateOf(false) }

    val fingerprint = remember(identitySecret) { identityFingerprint(identitySecret) }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(c.Background)
            .statusBarsPadding()
            .navigationBarsPadding()
            .verticalScroll(rememberScrollState())
            .testTag("settings-screen"),
    ) {
        // Top bar: back + title, hairline underline.
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 12.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                modifier = Modifier
                    .size(48.dp)
                    .clip(RoundedCornerShape(12.dp))
                    .clickable(onClick = onBack)
                    .testTag("settings-back")
                    .semantics { role = Role.Button; contentDescription = "Back" },
                contentAlignment = Alignment.Center,
            ) {
                Text("‹", color = c.Foreground, fontSize = 22.sp, fontWeight = FontWeight.Medium)
            }
            Text("Settings", color = c.Foreground, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
        }
        HairlineDivider()

        Column(modifier = Modifier.padding(horizontal = 16.dp)) {
            // ── Appearance ────────────────────────────────────────────────
            SectionHeader("Appearance")
            SettingsCard {
                RowLabel("Theme", "System follows your device's light or dark setting.")
                Spacer(Modifier.height(12.dp))
                SegmentedControl(
                    options = listOf(
                        ThemeMode.SYSTEM to "System",
                        ThemeMode.LIGHT to "Light",
                        ThemeMode.DARK to "Dark",
                    ),
                    selected = themeMode,
                    onSelect = onThemeModeChange,
                    testTag = "theme-segment",
                )
            }

            // ── Connection & devices ──────────────────────────────────────
            SectionHeader("Connection & devices")
            SettingsCard(contentPadding = 0.dp) {
                InsetRow {
                    Text("Status", color = c.Foreground, fontSize = 14.sp)
                    Spacer(Modifier.weight(1f))
                    ConnectionPill(phase)
                }
                RowDivider()
                InsetRow {
                    StatusDot(pillColor(phase), size = 8)
                    Spacer(Modifier.width(10.dp))
                    Column(Modifier.weight(1f)) {
                        Text(
                            deviceLabel.ifBlank { "This device" },
                            color = c.Foreground,
                            fontSize = 14.sp,
                            fontWeight = FontWeight.Medium,
                            overflow = TextOverflow.Ellipsis,
                            maxLines = 1,
                        )
                        Spacer(Modifier.height(2.dp))
                        Text(
                            "This device · ${phaseWord(phase)}",
                            color = c.MutedForeground,
                            fontSize = 12.sp,
                        )
                    }
                    Text("You", style = microLabel(), color = c.MutedForeground)
                }
                RowDivider()
                TappableRow(onClick = onPairNew, testTag = "pair-new-device") {
                    Column(Modifier.weight(1f)) {
                        Text("Pair a new device", color = c.Foreground, fontSize = 14.sp)
                        Spacer(Modifier.height(2.dp))
                        Text("Replaces this device's pairing.", color = c.MutedForeground, fontSize = 12.sp)
                    }
                    Chevron()
                }
                RowDivider()
                TappableRow(onClick = { confirmForget = true }, testTag = "forget-device") {
                    Text("Forget this device", color = c.StatusDanger, fontSize = 14.sp)
                    Spacer(Modifier.weight(1f))
                }
            }

            // ── Notifications ─────────────────────────────────────────────
            SectionHeader("Notifications")
            SettingsCard(contentPadding = 0.dp) {
                ToggleRow(
                    title = "Push notifications",
                    subtitle = "Register this device for Ping pushes.",
                    checked = pushEnabled,
                    onCheckedChange = { pushEnabled = it; settings.pushEnabled = it },
                    testTag = "push-toggle",
                )
                RowDivider()
                Column(modifier = Modifier.padding(horizontal = 14.dp, vertical = 12.dp)) {
                    RowLabel(
                        "Notify me about",
                        if (pushEnabled) null else "Turn on push notifications to choose.",
                    )
                    Spacer(Modifier.height(12.dp))
                    SegmentedControl(
                        options = listOf(
                            NotifyScope.REQUIRES_RESPONSE to "Needs a reply",
                            NotifyScope.ALL to "All pings",
                        ),
                        selected = notifyScope,
                        onSelect = { notifyScope = it; settings.notifyScope = it },
                        enabled = pushEnabled,
                        testTag = "notify-scope-segment",
                    )
                }
            }

            // ── Device label & identity ───────────────────────────────────
            SectionHeader("Device label & identity")
            SettingsCard {
                RowLabel("Device name", "Shown here and sent as this device's pairing label.")
                Spacer(Modifier.height(8.dp))
                HirselField(
                    value = labelDraft,
                    onValueChange = { labelDraft = sanitizeDeviceLabel(it) },
                    placeholder = "Device name",
                    testTag = "rename-field",
                    singleLine = true,
                    imeAction = ImeAction.Done,
                    onImeAction = { commitRename(labelDraft, deviceLabel, onRename, context) },
                )
                Spacer(Modifier.height(10.dp))
                Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    val changed = labelDraft.trim().isNotEmpty() && labelDraft.trim() != deviceLabel
                    TextButton(
                        onClick = { commitRename(labelDraft, deviceLabel, onRename, context) },
                        enabled = changed,
                        modifier = Modifier.testTag("rename-save"),
                    ) {
                        Text("Save", color = if (changed) c.AccentRing else c.MutedForeground, fontWeight = FontWeight.SemiBold)
                    }
                }
            }
            SettingsCard {
                RowLabel("Device identity", "A stable fingerprint of this device's iroh identity.")
                Spacer(Modifier.height(10.dp))
                CopyableMono(
                    value = fingerprint,
                    copyLabel = "iroh identity fingerprint",
                    testTag = "identity-value",
                )
            }
            SettingsCard {
                TappableRaw(onClick = { confirmReset = true }, testTag = "reset-identity") {
                    Column(Modifier.weight(1f)) {
                        Text("Reset local identity", color = c.StatusDanger, fontSize = 14.sp, fontWeight = FontWeight.Medium)
                        Spacer(Modifier.height(2.dp))
                        Text(
                            "Destroys this device's key and token. Forces re-pairing from scratch.",
                            color = c.MutedForeground,
                            fontSize = 12.sp,
                            lineHeight = 17.sp,
                        )
                    }
                }
            }

            // ── About / debug ─────────────────────────────────────────────
            SectionHeader("About & debug")
            SettingsCard(contentPadding = 0.dp) {
                InsetRow {
                    Text("App version", color = c.Foreground, fontSize = 14.sp)
                    Spacer(Modifier.weight(1f))
                    Text(appVersion, color = c.MutedForeground, fontSize = 13.sp, fontFamily = HirselMono)
                }
                RowDivider()
                InsetRow {
                    Text("Host version", color = c.Foreground, fontSize = 14.sp)
                    Spacer(Modifier.weight(1f))
                    Text(
                        hostVersion ?: "Not reported",
                        color = c.MutedForeground,
                        fontSize = 13.sp,
                        fontFamily = if (hostVersion != null) HirselMono else null,
                    )
                }
                RowDivider()
                ToggleRow(
                    title = "Debug mode",
                    subtitle = "Verbose client logging for diagnostics.",
                    checked = debugMode,
                    onCheckedChange = { debugMode = it; settings.debugMode = it },
                    testTag = "debug-toggle",
                )
                RowDivider()
                TappableRow(
                    onClick = {
                        copyToClipboard(
                            context,
                            "hirsel diagnostics",
                            buildDiagnostics(appVersion, hostVersion, themeMode, phase, pushEnabled, notifyScope, debugMode, deviceLabel, fingerprint),
                        )
                    },
                    testTag = "copy-diagnostics",
                ) {
                    Column(Modifier.weight(1f)) {
                        Text("Copy diagnostics", color = c.Foreground, fontSize = 14.sp)
                        Spacer(Modifier.height(2.dp))
                        Text("Version, connection, and settings — no secrets.", color = c.MutedForeground, fontSize = 12.sp)
                    }
                    CopyGlyph()
                }
            }

            Spacer(Modifier.height(28.dp))
        }
    }

    if (confirmForget) {
        DestructiveConfirmDialog(
            title = "Forget this device?",
            body = "Clears the stored token for “${deviceLabel.ifBlank { "this device" }}”. You'll need a new pairing link to reconnect.",
            confirmLabel = "Forget",
            onConfirm = onForget,
            onDismiss = { confirmForget = false },
            confirmTestTag = "forget-confirm",
        )
    }
    if (confirmReset) {
        DestructiveConfirmDialog(
            title = "Reset local identity?",
            body = "Permanently destroys this device's iroh key and device token. The host will no longer recognise it — you must pair again from a fresh link.",
            confirmLabel = "Reset identity",
            onConfirm = onResetIdentity,
            onDismiss = { confirmReset = false },
            confirmTestTag = "reset-confirm",
        )
    }
}

// ── Section building blocks ───────────────────────────────────────────────

@Composable
private fun SectionHeader(text: String) {
    Text(
        text.uppercase(Locale.US),
        style = microLabel(),
        color = LocalHirselColors.current.MutedForeground,
        modifier = Modifier.padding(top = 22.dp, bottom = 8.dp, start = 2.dp),
    )
}

@Composable
private fun SettingsCard(contentPadding: androidx.compose.ui.unit.Dp = 16.dp, content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit) {
    val c = LocalHirselColors.current
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = 10.dp)
            .clip(RoundedCornerShape(14.dp))
            .background(c.Card, RoundedCornerShape(14.dp))
            .border(1.dp, c.Border, RoundedCornerShape(14.dp))
            .padding(contentPadding),
        content = content,
    )
}

@Composable
private fun RowLabel(title: String, subtitle: String?) {
    val c = LocalHirselColors.current
    Text(title, color = c.Foreground, fontSize = 14.sp, fontWeight = FontWeight.Medium)
    if (subtitle != null) {
        Spacer(Modifier.height(2.dp))
        Text(subtitle, color = c.MutedForeground, fontSize = 12.sp, lineHeight = 17.sp)
    }
}

/** A row inset to match card padding when the card itself has zero padding. */
@Composable
private fun InsetRow(content: @Composable androidx.compose.foundation.layout.RowScope.() -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 14.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        content = content,
    )
}

@Composable
private fun TappableRow(onClick: () -> Unit, testTag: String, content: @Composable androidx.compose.foundation.layout.RowScope.() -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 14.dp, vertical = 13.dp)
            .testTag(testTag),
        verticalAlignment = Alignment.CenterVertically,
        content = content,
    )
}

/** Like [TappableRow] but for a card that already carries padding (single-row card). */
@Composable
private fun TappableRaw(onClick: () -> Unit, testTag: String, content: @Composable androidx.compose.foundation.layout.RowScope.() -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(role = Role.Button, onClick = onClick)
            .testTag(testTag),
        verticalAlignment = Alignment.CenterVertically,
        content = content,
    )
}

@Composable
private fun RowDivider() {
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .height(1.dp)
            .background(LocalHirselColors.current.Border),
    )
}

@Composable
private fun Chevron() {
    Text("›", color = LocalHirselColors.current.MutedForeground, fontSize = 20.sp)
}

@Composable
private fun CopyGlyph() {
    // Two offset rounded squares read as "copy" without an icon font.
    val c = LocalHirselColors.current
    Box(modifier = Modifier.size(18.dp)) {
        Box(
            modifier = Modifier
                .padding(start = 4.dp, top = 4.dp)
                .size(12.dp)
                .border(1.5.dp, c.MutedForeground, RoundedCornerShape(3.dp)),
        )
        Box(
            modifier = Modifier
                .size(12.dp)
                .background(c.Card, RoundedCornerShape(3.dp))
                .border(1.5.dp, c.MutedForeground, RoundedCornerShape(3.dp)),
        )
    }
}

// ── Segmented control ─────────────────────────────────────────────────────

@Composable
private fun <T> SegmentedControl(
    options: List<Pair<T, String>>,
    selected: T,
    onSelect: (T) -> Unit,
    enabled: Boolean = true,
    testTag: String,
) {
    val c = LocalHirselColors.current
    // 48dp tall so each segment clears the minimum touch target (C28).
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(48.dp)
            .clip(RoundedCornerShape(10.dp))
            .background(c.Secondary, RoundedCornerShape(10.dp))
            .border(1.dp, c.Border, RoundedCornerShape(10.dp))
            .padding(3.dp)
            .semantics { role = Role.RadioButton }
            .testTag(testTag),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        options.forEach { (value, label) ->
            val isSelected = value == selected
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxSize()
                    .clip(RoundedCornerShape(8.dp))
                    .background(
                        if (isSelected) c.Accent.copy(alpha = if (enabled) 0.16f else 0.07f) else Color.Transparent,
                        RoundedCornerShape(8.dp),
                    )
                    .selectable(
                        selected = isSelected,
                        enabled = enabled,
                        role = Role.RadioButton,
                        onClick = { onSelect(value) },
                    )
                    .semantics { stateDescription = if (isSelected) "Selected" else "Not selected" }
                    .testTag("segment-$label"),
                contentAlignment = Alignment.Center,
            ) {
                val textColor = when {
                    !enabled -> c.MutedForeground.copy(alpha = 0.5f)
                    isSelected -> c.Foreground
                    else -> c.MutedForeground
                }
                Text(
                    label,
                    color = textColor,
                    fontSize = 13.sp,
                    fontWeight = if (isSelected) FontWeight.SemiBold else FontWeight.Normal,
                    maxLines = 1,
                )
            }
        }
    }
}

/**
 * A settings row whose entire surface is a single Switch toggle for TalkBack: the
 * row carries the toggleable semantics (Role.Switch + on/off state) and the visual
 * [Switch] is decorative (onCheckedChange = null), so the whole row flips (C29).
 */
@Composable
private fun ToggleRow(
    title: String,
    subtitle: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    testTag: String,
) {
    val c = LocalHirselColors.current
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .toggleable(
                value = checked,
                role = Role.Switch,
                onValueChange = onCheckedChange,
            )
            .padding(horizontal = 14.dp, vertical = 12.dp)
            .testTag(testTag),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, color = c.Foreground, fontSize = 14.sp)
            Spacer(Modifier.height(2.dp))
            Text(subtitle, color = c.MutedForeground, fontSize = 12.sp)
        }
        Spacer(Modifier.width(12.dp))
        Switch(
            checked = checked,
            onCheckedChange = null,
            colors = SwitchDefaults.colors(
                checkedThumbColor = c.OnAccent,
                checkedTrackColor = c.Accent,
                checkedBorderColor = c.Accent,
                uncheckedThumbColor = c.MutedForeground,
                uncheckedTrackColor = c.Secondary,
                uncheckedBorderColor = c.InputBorder,
            ),
        )
    }
}

@Composable
private fun CopyableMono(value: String, copyLabel: String, testTag: String) {
    val c = LocalHirselColors.current
    val context = LocalContext.current
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(9.dp))
            .background(c.SurfaceRaised, RoundedCornerShape(9.dp))
            .border(1.dp, c.Border, RoundedCornerShape(9.dp))
            .clickable { copyToClipboard(context, copyLabel, value) }
            .padding(horizontal = 12.dp, vertical = 11.dp)
            .testTag(testTag),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            value,
            color = c.Foreground,
            fontSize = 13.sp,
            fontFamily = HirselMono,
            modifier = Modifier.weight(1f),
        )
        Spacer(Modifier.width(10.dp))
        CopyGlyph()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

private fun commitRename(draft: String, current: String, onRename: (String) -> Unit, context: Context) {
    val trimmed = draft.trim()
    if (trimmed.isEmpty() || trimmed == current) return
    onRename(trimmed)
    Toast.makeText(context, "Device renamed", Toast.LENGTH_SHORT).show()
}

@Composable
private fun pillColor(phase: Phase): Color {
    val c = LocalHirselColors.current
    return when (phase) {
        is Phase.Online -> c.StatusSuccess
        is Phase.Connecting -> c.StatusAttention
        is Phase.Reconnecting -> c.StatusAttention
        is Phase.Offline -> c.StatusIdle
        is Phase.Failed -> c.StatusDanger
    }
}

private fun phaseWord(phase: Phase): String = when (phase) {
    is Phase.Online -> "connected"
    is Phase.Connecting -> "connecting…"
    is Phase.Reconnecting -> "reconnecting…"
    is Phase.Offline -> "offline"
    is Phase.Failed -> "connection error"
}

/** Non-secret, one-way fingerprint of the iroh identity — safe to show + copy. */
private fun identityFingerprint(secret: String?): String {
    if (secret.isNullOrBlank()) return "unavailable"
    val digest = MessageDigest.getInstance("SHA-256").digest(secret.toByteArray())
    val hex = digest.take(10).joinToString("") { "%02X".format(it) }
    return hex.chunked(4).joinToString(" ")
}

private fun copyToClipboard(context: Context, label: String, value: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    clipboard.setPrimaryClip(ClipData.newPlainText(label, value))
    // Android 13+ shows its own copy confirmation; toast elsewhere for parity.
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
        Toast.makeText(context, "Copied", Toast.LENGTH_SHORT).show()
    }
}

private fun buildDiagnostics(
    appVersion: String,
    hostVersion: String?,
    themeMode: ThemeMode,
    phase: Phase,
    pushEnabled: Boolean,
    notifyScope: NotifyScope,
    debugMode: Boolean,
    deviceLabel: String,
    fingerprint: String,
): String = buildString {
    appendLine("hirsel diagnostics")
    appendLine("app version: $appVersion")
    appendLine("host version: ${hostVersion ?: "not reported"}")
    appendLine("device: ${Build.MANUFACTURER} ${Build.MODEL}")
    appendLine("android: ${Build.VERSION.RELEASE} (sdk ${Build.VERSION.SDK_INT})")
    appendLine("theme: ${themeMode.name.lowercase()}")
    appendLine("connection: ${phaseWord(phase)}")
    appendLine("push: ${if (pushEnabled) "on" else "off"}")
    appendLine("notify: ${notifyScope.name.lowercase()}")
    appendLine("debug: ${if (debugMode) "on" else "off"}")
    appendLine("device label: $deviceLabel")
    append("identity: $fingerprint")
}
