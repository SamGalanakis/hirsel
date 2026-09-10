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
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.hirsel.android.HirselField
import dev.hirsel.android.pairing.Connection
import dev.hirsel.android.ui.LocalHirselColors
import dev.hirsel.core.Thread
import dev.hirsel.core.ThreadKind

internal fun threadIconText(thread: Thread): String = thread.icon
    ?: thread.title.trim().let { if (it.isEmpty()) "#" else String(Character.toChars(it.codePointAt(0))).uppercase() }

internal fun threadIconError(icon: String?): String? {
    if (icon == null) return null
    if (icon.isBlank()) return "Choose an emoji or symbol, or use the default icon."
    if (icon.codePoints().anyMatch { Character.isISOControl(it) || it == 0x2028 || it == 0x2029 })
        return "Use an emoji or symbol without line breaks or control characters."
    if (icon.codePointCount(0, icon.length) > 16 || icon.toByteArray(Charsets.UTF_8).size > 64)
        return "Keep the icon to 16 characters or fewer."
    return null
}

@Composable
internal fun ThreadAvatar(thread: Thread) {
    val c = LocalHirselColors.current
    val tones = listOf(c.StatusSuccess, c.StatusAttention, c.Accent, c.StatusDanger, c.MutedForeground)
    val shape = if (thread.kind == ThreadKind.SPACE) RoundedCornerShape(6.dp) else RoundedCornerShape(50)
    Box(Modifier.size(28.dp).background(tones[(thread.id % 5uL).toInt()].copy(alpha = .16f), shape).clearAndSetSemantics { }, contentAlignment = Alignment.Center) {
        Text(threadIconText(thread), color = c.Foreground, fontSize = 14.sp, maxLines = 1)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ThreadIconPicker(thread: Thread, history: String, connection: Connection, onClose: () -> Unit) {
    var icon by remember(thread.id, thread.revision) { mutableStateOf(thread.icon) }
    var saveError by remember { mutableStateOf<String?>(null) }
    val c = LocalHirselColors.current
    val error = threadIconError(icon)
    val presets = listOf("🌱" to "Seedling", "🛠️" to "Tools", "📚" to "Books", "💡" to "Idea", "🚀" to "Rocket", "🎨" to "Art", "🏡" to "Home", "🧭" to "Compass", "🛒" to "Shopping", "🌍" to "Globe", "🎯" to "Target", "⭐" to "Star")
    AlertDialog(onDismissRequest = onClose, title = { Text("Change thread icon") }, text = {
        Column(Modifier.heightIn(max = 400.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                ThreadAvatar(thread.copy(icon = icon))
                Text(thread.title)
            }
            FlowRow {
                presets.forEach { (symbol, label) ->
                    TextButton(onClick = { icon = symbol }) { Text(symbol, modifier = Modifier.clearAndSetSemantics { contentDescription = label }) }
                }
            }
            HirselField(value = icon.orEmpty(), onValueChange = { icon = it }, placeholder = "Custom emoji or symbol", testTag = "thread-icon-input", contentDescription = "Custom emoji or symbol", singleLine = true)
            if (error != null) Text(error, color = c.StatusDanger)
            if (saveError != null) Text(saveError!!, color = c.StatusDanger)
            TextButton(onClick = { icon = null }) { Text("Use default") }
        }
    }, confirmButton = {
        TextButton(enabled = error == null && connection.isOnline, onClick = {
            val accepted = runCatching { connection.updateThreadIcon(history, thread.id, icon, thread.revision) }
            if (accepted.getOrDefault(false)) onClose()
            else saveError = accepted.exceptionOrNull()?.message ?: "Thread or connection changed. Close and reopen the icon picker to try again."
        }) { Text("Save icon") }
    }, dismissButton = { TextButton(onClick = onClose) { Text("Cancel") } })
}
