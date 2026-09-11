package dev.hirsel.android.chat

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import dev.hirsel.android.HairlineDivider
import dev.hirsel.android.HirselField
import dev.hirsel.android.pairing.Connection
import dev.hirsel.android.ui.LocalHirselColors
import dev.hirsel.core.Thread as WorkThread
import dev.hirsel.core.ThreadKind
import org.json.JSONArray
import org.json.JSONObject
import org.json.JSONTokener

/** Constrained native rendering; the host remains the action-contract authority. */
@Composable
internal fun ThreadInstrument(thread: WorkThread, historyId: String, connection: Connection) {
    val root = remember(thread.instrumentJson) { runCatching { JSONTokener(thread.instrumentJson).nextValue() }.getOrNull() }
    val fields = remember(thread.id, thread.revision) { mutableStateMapOf<String, String>().apply { putAll(initialFields(root)) } }
    val enabled = thread.settledAt == null && thread.archivedAt == null && !snoozed(thread.snoozedUntil)
    val submit: (String, JSONObject) -> Unit = { action, data -> connection.action(historyId, thread.id, action, data.toString(), thread.revision) }
    Column(Modifier.fillMaxWidth()) { InstrumentNode(root, fields, enabled, thread.kind, submit) }
}

private fun snoozed(until: String?): Boolean = until?.let { runCatching { java.time.Instant.parse(it).isAfter(java.time.Instant.now()) }.getOrDefault(false) } ?: false

internal fun instrumentActionVisible(settles: Boolean, kind: ThreadKind): Boolean =
    kind == ThreadKind.TASK || !settles

private fun initialFields(node: Any?): Map<String, String> = when (node) {
    is JSONArray -> (0 until node.length()).flatMap { initialFields(node.opt(it)).entries }.associate { it.toPair() }
    is JSONObject -> if (node.optString("type") == "field") mapOf(node.optString("name") to node.optString("value", "")) else initialFields(node.optJSONArray("children"))
    else -> emptyMap()
}

@Composable
private fun InstrumentNode(node: Any?, fields: MutableMap<String, String>, enabled: Boolean, kind: ThreadKind, submit: (String, JSONObject) -> Unit) {
    val colors = LocalHirselColors.current
    when (node) {
        is JSONArray -> repeat(node.length()) { InstrumentNode(node.opt(it), fields, enabled, kind, submit) }
        is JSONObject -> when (node.optString("type")) {
            "card", "inset" -> Column { InstrumentNode(node.optJSONArray("children"), fields, enabled, kind, submit) }
            "text", "eyebrow" -> Text(node.optString("text"), color = colors.Foreground)
            "heading" -> Text(node.optString("text"), color = colors.Foreground, fontWeight = FontWeight.SemiBold)
            "badge", "status" -> Text(node.optString("label"), color = colors.MutedForeground)
            "divider" -> HairlineDivider()
            "keyValue" -> node.optJSONArray("items")?.let { rows -> repeat(rows.length()) { i -> rows.optJSONObject(i)?.let { Text("${it.optString("label")}: ${it.optString("value")}", color = colors.Foreground) } } }
            "field" -> {
                val name = node.optString("name")
                Text(node.optString("label", name), color = colors.MutedForeground)
                HirselField(value = fields[name].orEmpty(), onValueChange = { fields[name] = it }, placeholder = node.optString("placeholder"), testTag = "thread-field-$name", singleLine = false)
            }
            "submit" -> if (instrumentActionVisible(node.optBoolean("settles", true), kind)) Button(onClick = { submit(node.optString("action", "submit"), JSONObject(fields.toMap())) }, enabled = enabled) { Text(node.optString("label")) }
            "optionList" -> if (instrumentActionVisible(node.optBoolean("settles", true), kind)) node.optJSONArray("options")?.let { options -> repeat(options.length()) { i -> options.optJSONObject(i)?.let { option ->
                Button(onClick = { submit(node.optString("action", "choose"), JSONObject().put("choice", option.optString("key")).put("label", option.optString("label"))) }, enabled = enabled) { Text(option.optString("label")) }
                if (option.has("detail")) Text(option.optString("detail"), color = colors.MutedForeground)
            } } }
            "viewSlot" -> {
                if (node.has("title")) Text(node.optString("title"), color = colors.Foreground)
                Text("This embedded view is available in the web app.", color = colors.MutedForeground)
            }
            else -> Text("This instrument component is unavailable on this device.", color = colors.MutedForeground)
        }
    }
}

/** Preserve all event payloads in Rust; show prose and legible execution progress here. */
@Composable
internal fun StreamTimeline(eventsJson: String) {
    val events = remember(eventsJson) { runCatching { JSONArray(eventsJson) }.getOrDefault(JSONArray()) }
    val colors = LocalHirselColors.current
    val prose = buildString { repeat(events.length()) { i -> events.optJSONObject(i)?.let { if (it.optString("kind") == "prose") append(it.optString("text")) } } }
    if (prose.isNotEmpty()) Text(prose, color = colors.Foreground)
    repeat(events.length()) { i -> events.optJSONObject(i)?.let { event ->
        val label = when (event.optString("kind")) {
            "tool_start" -> "Running ${event.optString("name")}"
            "tool_done" -> "${event.optString("name")}: ${if (event.optBoolean("ok")) "done" else "failed"}"
            "code_start" -> event.optString("code")
            "code_done" -> if (event.optBoolean("ok")) "Code completed" else "Code failed"
            else -> null
        }
        label?.let { Text(it, color = colors.MutedForeground) }
    } }
}
