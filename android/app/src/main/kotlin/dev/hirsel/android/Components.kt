package dev.hirsel.android

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.paneTitle
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import dev.hirsel.android.pairing.Phase
import dev.hirsel.android.ui.HirselMono
import dev.hirsel.android.ui.LocalHirselColors

// ---------------------------------------------------------------------------
// Shared overlays
// ---------------------------------------------------------------------------

/**
 * A focused destructive confirm on a dimmed scrim — the one place a real overlay
 * is warranted. Reused for "forget device" and "reset identity" in Settings.
 */
@Composable
internal fun DestructiveConfirmDialog(
    title: String,
    body: String,
    confirmLabel: String,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
    confirmTestTag: String = "destructive-confirm",
) {
    val c = LocalHirselColors.current
    // Predictable dismissal: back press and scrim tap both cancel; the dialog
    // window traps accessibility focus so its controls are reachable (C21).
    Dialog(
        onDismissRequest = onDismiss,
        properties = DialogProperties(
            dismissOnBackPress = true,
            dismissOnClickOutside = true,
        ),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .shadow(24.dp, RoundedCornerShape(16.dp), clip = false)
                .clip(RoundedCornerShape(16.dp))
                .background(c.Card, RoundedCornerShape(16.dp))
                .border(1.dp, c.Border, RoundedCornerShape(16.dp))
                .padding(20.dp)
                .semantics(mergeDescendants = false) { paneTitle = title }
                .testTag("confirm-dialog"),
        ) {
            Text(title, fontWeight = FontWeight.SemiBold, color = c.Foreground, fontSize = 16.sp)
            Spacer(Modifier.height(8.dp))
            Text(body, color = c.MutedForeground, fontSize = 13.sp, lineHeight = 20.sp)
            Spacer(Modifier.height(20.dp))
            Row(horizontalArrangement = Arrangement.End, modifier = Modifier.fillMaxWidth()) {
                TextButton(onClick = onDismiss) { Text("Cancel", color = c.MutedForeground) }
                Spacer(Modifier.width(6.dp))
                TextButton(
                    onClick = { onConfirm(); onDismiss() },
                    modifier = Modifier.testTag(confirmTestTag),
                ) {
                    Text(confirmLabel, color = c.StatusDanger, fontWeight = FontWeight.SemiBold)
                }
            }
        }
    }
}

/** Minimal 8-tooth gear drawn with Canvas (no icon-font dependency). */
@Composable
internal fun GearGlyph(color: androidx.compose.ui.graphics.Color, modifier: Modifier = Modifier) {
    Canvas(modifier = modifier) {
        val cx = size.width / 2f
        val cy = size.height / 2f
        val outer = size.minDimension * 0.46f
        val inner = size.minDimension * 0.34f
        val hole = size.minDimension * 0.17f
        val stroke = size.minDimension * 0.11f
        // Teeth as short radial spokes.
        for (i in 0 until 8) {
            val a = Math.toRadians((i * 45).toDouble())
            val sx = cx + (inner * Math.cos(a)).toFloat()
            val sy = cy + (inner * Math.sin(a)).toFloat()
            val ex = cx + (outer * Math.cos(a)).toFloat()
            val ey = cy + (outer * Math.sin(a)).toFloat()
            drawLine(color, Offset(sx, sy), Offset(ex, ey), stroke, androidx.compose.ui.graphics.StrokeCap.Round)
        }
        // Gear body ring.
        drawCircle(color, radius = inner, center = Offset(cx, cy), style = androidx.compose.ui.graphics.drawscope.Stroke(width = stroke))
        // Hub punch-out (redraw hole via a filled circle in the surface... approximated with a thinner ring).
        drawCircle(color, radius = hole, center = Offset(cx, cy), style = androidx.compose.ui.graphics.drawscope.Stroke(width = stroke * 0.85f))
    }
}

@Composable
internal fun ConnectionPill(phase: Phase) {
    val (color, text) = when (phase) {
        is Phase.Online -> LocalHirselColors.current.StatusSuccess to "online"
        is Phase.Connecting -> LocalHirselColors.current.StatusAttention to "connecting"
        is Phase.Reconnecting -> LocalHirselColors.current.StatusAttention to "reconnecting"
        is Phase.Offline -> LocalHirselColors.current.StatusIdle to "offline"
        is Phase.Failed -> LocalHirselColors.current.StatusDanger to "error"
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .background(LocalHirselColors.current.Secondary, RoundedCornerShape(9999.dp))
            .padding(horizontal = 8.dp, vertical = 3.dp)
            .testTag("connection-status")
            .semantics { contentDescription = "Connection $text" },
    ) {
        StatusDot(color, size = 7)
        Spacer(Modifier.width(6.dp))
        Text(text, color = LocalHirselColors.current.MutedForeground, fontSize = 11.sp, fontWeight = FontWeight.Medium)
    }
}

// ---------------------------------------------------------------------------
// Shared building blocks
// ---------------------------------------------------------------------------

@Composable
internal fun microLabel() = androidx.compose.material3.MaterialTheme.typography.labelMedium.copy(
    letterSpacing = 0.5.sp,
)

@Composable
internal fun StatusDot(color: androidx.compose.ui.graphics.Color, size: Int = 10) {
    Box(
        modifier = Modifier
            .size(size.dp)
            .background(color, RoundedCornerShape(9999.dp)),
    )
}

@Composable
internal fun HairlineDivider(label: String? = null) {
    if (label == null) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .height(1.dp)
                .background(LocalHirselColors.current.Border),
        )
    } else {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(modifier = Modifier.weight(1f).height(1.dp).background(LocalHirselColors.current.Border))
            Text(
                label,
                color = LocalHirselColors.current.MutedForeground,
                fontSize = 11.sp,
                modifier = Modifier.padding(horizontal = 10.dp),
            )
            Box(modifier = Modifier.weight(1f).height(1.dp).background(LocalHirselColors.current.Border))
        }
    }
}

@Composable
internal fun HirselField(
    value: String,
    onValueChange: (String) -> Unit,
    placeholder: String,
    testTag: String,
    singleLine: Boolean,
    mono: Boolean = false,
    isError: Boolean = false,
    imeAction: ImeAction = ImeAction.Default,
    onImeAction: (() -> Unit)? = null,
    contentDescription: String? = null,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        placeholder = {
            Text(
                placeholder,
                color = LocalHirselColors.current.MutedForeground.copy(alpha = 0.7f),
                fontSize = if (mono) 12.sp else 14.sp,
                fontFamily = if (mono) HirselMono else androidx.compose.ui.text.font.FontFamily.Default,
            )
        },
        isError = isError,
        singleLine = singleLine,
        maxLines = if (singleLine) 1 else 3,
        textStyle = androidx.compose.material3.MaterialTheme.typography.bodyMedium.copy(
            color = LocalHirselColors.current.Foreground,
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
            focusedBorderColor = LocalHirselColors.current.AccentRing,
            unfocusedBorderColor = LocalHirselColors.current.InputBorder,
            errorBorderColor = LocalHirselColors.current.StatusDanger,
            errorCursorColor = LocalHirselColors.current.StatusDanger,
            focusedContainerColor = androidx.compose.ui.graphics.Color.Transparent,
            unfocusedContainerColor = androidx.compose.ui.graphics.Color.Transparent,
            errorContainerColor = androidx.compose.ui.graphics.Color.Transparent,
            cursorColor = LocalHirselColors.current.AccentRing,
            focusedTextColor = LocalHirselColors.current.Foreground,
            unfocusedTextColor = LocalHirselColors.current.Foreground,
            errorTextColor = LocalHirselColors.current.Foreground,
        ),
        shape = RoundedCornerShape(8.dp),
        modifier = Modifier
            .fillMaxWidth()
            .testTag(testTag)
            .then(if (contentDescription != null) Modifier.semantics { this.contentDescription = contentDescription } else Modifier),
    )
}
