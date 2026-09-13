package dev.hirsel.android.chat

import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.hirsel.android.pairing.Connection
import dev.hirsel.android.ui.LocalHirselColors
import dev.hirsel.core.Thread
import dev.hirsel.core.ThreadIcon
import dev.hirsel.core.ThreadKind
import dev.hirsel.core.ThreadTint

/** The default mark: initials of the first two words, or the first letter. */
internal fun threadMonogram(title: String): String {
    val words = title.trim().split(Regex("\\s+")).filter { it.isNotEmpty() }
    if (words.isEmpty()) return "#"
    return words.take(if (words.size > 1) 2 else 1)
        .joinToString("") { String(Character.toChars(it.codePointAt(0))) }
        .uppercase()
}

/** The monogram a Thread shows when it has no icon of its own. */
internal fun threadIconText(thread: Thread): String = threadMonogram(thread.title)

/** Tile and glyph colours per tint, mirroring the --tint-* tokens in app/src/styles.css. */
internal fun threadTintColors(tint: ThreadTint, isLight: Boolean): Pair<Color, Color> {
    val light = when (tint) {
        ThreadTint.NEUTRAL -> 0xFFE4F0EE to 0xFF49666A
        ThreadTint.RED -> 0xFFFCE0E0 to 0xFF763638
        ThreadTint.ORANGE -> 0xFFFAE2D8 to 0xFF743B1F
        ThreadTint.AMBER -> 0xFFF5E5D3 to 0xFF6A4400
        ThreadTint.GREEN -> 0xFFD8EEE0 to 0xFF085634
        ThreadTint.TEAL -> 0xFFD2EFEC to 0xFF005652
        ThreadTint.BLUE -> 0xFFD3EDF6 to 0xFF005269
        ThreadTint.VIOLET -> 0xFFEBE4FA to 0xFF4F3B71
        ThreadTint.PINK -> 0xFFF9E0EB to 0xFF6B314E
    }
    val dark = when (tint) {
        ThreadTint.NEUTRAL -> 0xFF162023 to 0xFF95AAA4
        ThreadTint.RED -> 0xFF361F1F to 0xFFF2A7A6
        ThreadTint.ORANGE -> 0xFF352118 to 0xFFEFAC8D
        ThreadTint.AMBER -> 0xFF312412 to 0xFFE8BB82
        ThreadTint.GREEN -> 0xFF172C20 to 0xFF8BD0A8
        ThreadTint.TEAL -> 0xFF0F2C2A to 0xFF73D1CA
        ThreadTint.BLUE -> 0xFF112A32 to 0xFF78CBE7
        ThreadTint.VIOLET -> 0xFF292335 to 0xFFCDB7F6
        ThreadTint.PINK -> 0xFF341F29 to 0xFFF1ACCC
    }
    val (background, foreground) = if (isLight) light else dark
    return Color(background) to Color(foreground)
}

@Composable
internal fun ThreadAvatar(thread: Thread) {
    val c = LocalHirselColors.current
    val symbol = thread.icon as? ThreadIcon.Symbol
    val (tile, glyph) = threadTintColors(symbol?.tint ?: ThreadTint.NEUTRAL, c.isLight)
    val shape = if (thread.kind == ThreadKind.SPACE) RoundedCornerShape(6.dp) else CircleShape
    Box(Modifier.size(28.dp).background(tile, shape).clearAndSetSemantics { }, contentAlignment = Alignment.Center) {
        val drawable = symbol?.let { THREAD_SYMBOL_DRAWABLES[it.name] }
        if (drawable != null) {
            Icon(painterResource(drawable), contentDescription = null, tint = glyph, modifier = Modifier.size(17.dp))
        } else {
            Text(threadIconText(thread), color = glyph, fontSize = 13.sp, maxLines = 1)
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ThreadIconPicker(thread: Thread, history: String, connection: Connection, onClose: () -> Unit) {
    val saved = thread.icon as? ThreadIcon.Symbol
    var symbol by remember(thread.id, thread.revision) { mutableStateOf(saved?.name) }
    var tint by remember(thread.id, thread.revision) { mutableStateOf(saved?.tint ?: ThreadTint.NEUTRAL) }
    var saveError by remember { mutableStateOf<String?>(null) }
    val c = LocalHirselColors.current
    val chosen = symbol?.let { ThreadIcon.Symbol(it, tint) }
    AlertDialog(onDismissRequest = onClose, title = { Text("Change thread icon") }, text = {
        Column(Modifier.heightIn(max = 400.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                ThreadAvatar(thread.copy(icon = chosen))
                Text(thread.title)
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                ThreadTint.entries.forEach { option ->
                    val (tile, _) = threadTintColors(option, c.isLight)
                    TextButton(onClick = { tint = option }) {
                        Box(
                            Modifier.size(if (option == tint) 22.dp else 18.dp).background(tile, CircleShape)
                                .clearAndSetSemantics { contentDescription = option.name.lowercase() }
                        )
                    }
                }
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(2.dp)) {
                THREAD_SYMBOL_DRAWABLES.forEach { (name, drawable) ->
                    val (_, glyph) = threadTintColors(if (name == symbol) tint else ThreadTint.NEUTRAL, c.isLight)
                    TextButton(onClick = { symbol = name }) {
                        Icon(
                            painterResource(drawable), contentDescription = null, tint = glyph,
                            modifier = Modifier.size(if (name == symbol) 22.dp else 18.dp)
                                .clearAndSetSemantics { contentDescription = name }
                        )
                    }
                }
            }
            if (saveError != null) Text(saveError!!, color = c.StatusDanger)
            TextButton(onClick = { symbol = null; tint = ThreadTint.NEUTRAL }) { Text("Use default") }
        }
    }, confirmButton = {
        TextButton(enabled = connection.isOnline, onClick = {
            val accepted = runCatching { connection.updateThreadIcon(history, thread.id, chosen, thread.revision) }
            if (accepted.getOrDefault(false)) onClose()
            else saveError = accepted.exceptionOrNull()?.message ?: "Thread or connection changed. Close and reopen the icon picker to try again."
        }) { Text("Save icon") }
    }, dismissButton = { TextButton(onClick = onClose) { Text("Cancel") } })
}
