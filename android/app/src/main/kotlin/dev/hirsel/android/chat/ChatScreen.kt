package dev.hirsel.android.chat

import androidx.activity.compose.BackHandler
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.hirsel.android.ConnectionPill
import dev.hirsel.android.GearGlyph
import dev.hirsel.android.HairlineDivider
import dev.hirsel.android.HirselField
import dev.hirsel.android.StatusDot
import dev.hirsel.android.microLabel
import dev.hirsel.android.pairing.Connection
import dev.hirsel.android.pairing.FailedSend
import dev.hirsel.android.pairing.Phase
import dev.hirsel.android.pairing.visibleIn
import dev.hirsel.android.ui.ErrorCopy
import dev.hirsel.android.ui.HirselMono
import dev.hirsel.android.ui.LocalHirselColors
import dev.hirsel.core.AgentActivityState
import dev.hirsel.core.Blob
import dev.hirsel.core.ChatAuthor
import dev.hirsel.core.ChatMessage
import dev.hirsel.core.ToolCall
import dev.hirsel.core.ThreadKind
import kotlinx.coroutines.launch

/** Durable thread inventory and focused, independently owned conversation. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun ChatScreen(connection: Connection, onOpenSettings: () -> Unit) {
    val snapshot = connection.snapshot
    val displayedHistory = snapshot?.historyId
    val c = LocalHirselColors.current
    val focused = connection.focusedThreadId
    BackHandler(enabled = focused != null) { connection.focusedThreadId = null }
    val thread = snapshot?.threads?.find { it.id == focused }
    val messages = snapshot?.messages.orEmpty().filter { it.threadId == focused }
    val stream = snapshot?.streams?.find { it.threadId == focused && !it.finished }
    val thinking = stream?.activity?.state == AgentActivityState.THINKING
    val executing = snapshot?.turns.orEmpty().any { it.threadId == focused && it.state in listOf("queued", "running") }
    var iconTarget by remember { mutableStateOf<Pair<String, dev.hirsel.core.Thread>?>(null) }
    LaunchedEffect(snapshot?.historyId) { iconTarget = null }
    iconTarget?.let { selected -> ThreadIconPicker(selected.second, selected.first, connection) { iconTarget = null } }
    var title by remember { mutableStateOf("") }
    var inventory by remember { mutableStateOf("Active") }
    val draft = focused?.let { connection.drafts[it] }.orEmpty()
    val send = {
        if (focused != null && draft.isNotBlank()) {
            connection.send(draft.trim(), focused, snapshot?.historyId)
            connection.drafts[focused] = ""
        }
    }
    Column(Modifier.fillMaxSize().statusBarsPadding().navigationBarsPadding().imePadding().padding(16.dp)) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            if (focused != null) Button(onClick = { connection.focusedThreadId = null }) { Text("Spaces & Tasks") }
            else Text("Spaces & Tasks", color = c.Foreground, fontSize = 20.sp)
            Spacer(Modifier.weight(1f))
            ConnectionPill(connection.phase)
            GearButton(onOpenSettings)
        }
        connection.actionError?.takeIf { it.visibleIn(displayedHistory, focused) }?.let { error ->
            Text(error.detail, color = c.StatusDanger, modifier = Modifier.clickable { connection.actionError = null })
        }
        when (val phase = connection.phase) {
            is Phase.Reconnecting -> ConnectionBanner("Reconnecting to your host…")
            is Phase.Offline -> ConnectionBanner(ErrorCopy.connection(phase.reason))
            is Phase.Failed -> ConnectionBanner(ErrorCopy.connection(phase.detail))
            else -> Unit
        }
        if (connection.recoveredDrafts.isNotEmpty()) {
            Text("Recovered drafts — choose a Thread, then restore text", color = c.Foreground)
            connection.recoveredDrafts.toList().forEach { recovered ->
                Text(recovered, maxLines = 3, color = c.MutedForeground)
                Button(enabled = focused != null, onClick = {
                    if (focused != null) {
                        connection.drafts[focused] = connection.drafts[focused].orEmpty().let { if (it.isBlank()) recovered else "$it\n$recovered" }
                        connection.recoveredDrafts.remove(recovered)
                    }
                }) { Text("Restore to draft") }
            }
        }
        if (focused == null) {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                HirselField(value = title, onValueChange = { title = it }, placeholder = "Name this Space or Task", testTag = "new-thread-title", singleLine = true)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = { displayedHistory?.let { connection.createThread(it, title.trim(), ThreadKind.SPACE, null) }; title = "" }, enabled = title.isNotBlank() && connection.isOnline && displayedHistory != null, modifier = Modifier.weight(1f)) { Text("New Space") }
                    Button(onClick = { displayedHistory?.let { connection.createThread(it, title.trim(), ThreadKind.TASK, null) }; title = "" }, enabled = title.isNotBlank() && connection.isOnline && displayedHistory != null, modifier = Modifier.weight(1f)) { Text("New Task") }
                }
            }
            FlowRow {
                listOf("Active", "Done", "Snoozed", "Archived").forEach { filter ->
                    ReplyChip(if (inventory == filter) "• $filter" else filter) { inventory = filter }
                }
            }
            LazyColumn(Modifier.weight(1f).testTag("thread-list")) {
                items(threadRows(snapshot?.threads.orEmpty().filter { t ->
                    when (inventory) {
                        "Archived" -> t.archivedAt != null
                        "Done" -> t.archivedAt == null && t.kind == ThreadKind.TASK && t.settledAt != null
                        "Snoozed" -> t.archivedAt == null && t.settledAt == null && isSnoozed(t.snoozedUntil)
                        else -> t.archivedAt == null && t.settledAt == null && !isSnoozed(t.snoozedUntil)
                    }
                }), key = { it.thread.id.toString() }) { row ->
                    val t = row.thread
                    Column(Modifier.fillMaxWidth().clickable { connection.openThread(t.id) }.padding(start = (row.depth.coerceAtMost(6) * 12).dp, top = 12.dp, bottom = 12.dp).testTag("thread-${t.id}")) {
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) { ThreadAvatar(t); Text(t.title, color = c.Foreground, fontWeight = FontWeight.SemiBold); Text(if (t.kind == ThreadKind.SPACE) "Space" else "Task", color = c.MutedForeground, fontSize = 12.sp) }
                        t.parentThreadId?.let { parent -> Text("In ${snapshot?.threads?.find { it.id == parent }?.title ?: "Thread #$parent"}", color = c.MutedForeground) }
                        if (t.parentThreadId == null && t.pinnedAt != null) Text("Pinned", color = c.MutedForeground)
                        Text(if (t.needsOwner) "Needs you" else if (!t.read) "Unread" else "Open", color = c.MutedForeground)
                    }
                    HairlineDivider()
                }
            }
        } else {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                if (thread != null) ThreadAvatar(thread)
                Text(thread?.title ?: "Thread #$focused", color = c.Foreground, fontSize = 18.sp)
                if (thread != null) Text(if (thread.kind == ThreadKind.SPACE) "Space" else "Task", color = c.MutedForeground, fontSize = 12.sp)
            }
            if (thread != null) {
                FlowRow {
                    ReplyChip("Change ${if (thread.kind == ThreadKind.SPACE) "space" else "task"} icon") { snapshot.historyId?.let { iconTarget = it to thread.copy() } }
                    thread.parentThreadId?.let { parent -> ReplyChip("Parent: ${snapshot.threads.find { it.id == parent }?.title ?: "#$parent"}") { connection.openThread(parent) } }
                    if (thread.parentThreadId == null) ReplyChip(if (thread.pinnedAt == null) "Pin" else "Unpin") { displayedHistory?.let { connection.action(it, focused, if (thread.pinnedAt == null) "pin" else "unpin", revision = thread.revision) } }
                    if (thread.kind == ThreadKind.TASK) ReplyChip(if (thread.settledAt == null) "Mark done" else "Reopen") { displayedHistory?.let { connection.action(it, focused, if (thread.settledAt == null) "settle" else "reopen") } }
                    if (thread.kind == ThreadKind.SPACE) ReplyChip("Change to Task") { displayedHistory?.let { connection.action(it, focused, "set_kind", org.json.JSONObject().put("kind", "task").toString(), thread.revision) } }
                    if (thread.kind == ThreadKind.TASK && thread.settledAt == null) ReplyChip("Change to Space") { displayedHistory?.let { connection.action(it, focused, "set_kind", org.json.JSONObject().put("kind", "space").toString(), thread.revision) } }
                    if (!thread.read) ReplyChip("Mark read") { displayedHistory?.let { connection.action(it, focused, "read") } }
                    ReplyChip(if (thread.archivedAt == null) "Archive" else "Unarchive") { displayedHistory?.let { connection.action(it, focused, if (thread.archivedAt == null) "archive" else "unarchive") } }
                    ReplyChip(if (isSnoozed(thread.snoozedUntil)) "Unsnooze" else "Snooze 1h") {
                        if (isSnoozed(thread.snoozedUntil)) displayedHistory?.let { connection.action(it, focused, "unsnooze") }
                        else displayedHistory?.let { connection.action(it, focused, "snooze", org.json.JSONObject().put("until", java.time.Instant.now().plusSeconds(3600).toString()).toString()) }
                    }
                }
            }
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                HirselField(value = title, onValueChange = { title = it }, placeholder = if (thread?.kind == ThreadKind.TASK) "Name this Task" else "Name this child", testTag = "new-child-title", singleLine = true)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (thread?.kind == ThreadKind.SPACE) Button(onClick = { displayedHistory?.let { connection.createThread(it, title.trim(), ThreadKind.SPACE, focused) }; title = "" }, enabled = title.isNotBlank() && connection.isOnline && displayedHistory != null, modifier = Modifier.weight(1f)) { Text("New Space") }
                    Button(onClick = { displayedHistory?.let { connection.createThread(it, title.trim(), ThreadKind.TASK, focused) }; title = "" }, enabled = title.isNotBlank() && connection.isOnline && displayedHistory != null, modifier = Modifier.weight(1f)) { Text("New Task") }
                }
            }
            FlowRow { snapshot?.threads.orEmpty().filter { it.parentThreadId == focused }.forEach { child -> ReplyChip("${threadIconText(child)} ${child.title}") { connection.openThread(child.id) } } }
            LazyColumn(Modifier.weight(1f).testTag("chat-list")) {
                snapshot?.briefs?.find { it.threadId == focused }?.let { brief ->
                    if (brief.text.isNotBlank()) item { Text("Current brief", color = c.MutedForeground); Text(brief.text, color = c.Foreground); if (brief.artifactIds.isNotEmpty()) Text("Artifacts: ${brief.artifactIds.joinToString { "#$it" }}", color = c.MutedForeground) }
                }
                if (thread != null) item {
                    displayedHistory?.let { ThreadInstrument(thread, it, connection) }
                }
                if (snapshot?.openedThreads?.contains(focused) != true) item { Text("Loading conversation…", color = c.MutedForeground) }
                if (snapshot?.historyHasMore?.contains(focused) == true) item {
                    Button(onClick = { connection.openThread(focused, messages.mapNotNull { it.id }.minOrNull()) }) { Text("Load earlier messages") }
                }
                items(messages, key = { "message-${it.id ?: it.clientId}" }) { message ->
                    Spacer(Modifier.height(8.dp)); MessageRow(message)
                    if (message.artifactIds.isNotEmpty()) Text("About artifacts ${message.artifactIds.joinToString { "#$it" }}", color = c.MutedForeground)
                    message.error?.let { error ->
                        Text(error, color = c.StatusDanger)
                        Button(onClick = { message.clientId?.let { connection.client?.retrySend(it) } }) { Text("Retry") }
                    }
                    if (message.mentions.isNotEmpty()) FlowRow { message.mentions.forEach { id -> ReplyChip("#$id") { connection.openThread(id) } } }
                }
                items(connection.failedSends.filter { it.threadId == focused }, key = { "failed-${it.id}" }) { failed -> FailedMessageRow(failed) { connection.retry(failed) } }
                if (stream != null) item {
                    StreamTimeline(stream.eventsJson)
                    if (thinking) WorkingRow(stream.activity.text)
                }
                if ((stream != null || executing) && displayedHistory != null) item { Button(onClick = { connection.stop(displayedHistory, focused) }) { Text("Stop") } }
                items(snapshot?.activities.orEmpty().filter { it.threadId == focused }, key = { "activity-${it.id}" }) { activity ->
                    if (activity.kind == "child_report") {
                        val report = runCatching { org.json.JSONObject(activity.dataJson) }.getOrNull()
                        val childId = report?.optString("child_thread_id")?.toULongOrNull()
                        Text(report?.optString("summary").orEmpty(), color = c.Foreground)
                        if (childId != null) ReplyChip("${snapshot?.threads?.find { it.id == childId }?.title ?: "Child #$childId"}: ${report.optString("status")}") { connection.openThread(childId) }
                    } else Text(activity.kind.replace('_', ' '), color = c.MutedForeground, fontSize = 11.sp)
                }
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.weight(1f)) { HirselField(value = draft, onValueChange = { connection.drafts[focused] = it }, placeholder = "Message this thread", testTag = "message-composer", singleLine = false, imeAction = ImeAction.Send, onImeAction = send) }
                Button(onClick = send, enabled = draft.isNotBlank()) { Text("Send") }
            }
        }
    }
}

private fun isSnoozed(until: String?): Boolean = until?.let { runCatching { java.time.Instant.parse(it).isAfter(java.time.Instant.now()) }.getOrDefault(false) } ?: false

/** A quiet, tappable gear affordance in the chat top bar — the entry to Settings.
 *  Sized to a 48dp touch target while keeping the glyph small (C28). */
@Composable
private fun GearButton(onClick: () -> Unit) {
    val c = LocalHirselColors.current
    Box(
        modifier = Modifier
            .size(48.dp)
            .clip(RoundedCornerShape(12.dp))
            .clickable(onClick = onClick)
            .testTag("open-settings")
            .semantics { contentDescription = "Settings" },
        contentAlignment = Alignment.Center,
    ) {
        GearGlyph(color = c.MutedForeground, modifier = Modifier.size(19.dp))
    }
}

/** A subtle full-width banner carrying friendly connection copy (never raw errors). */
@Composable
private fun ConnectionBanner(text: String) {
    val c = LocalHirselColors.current
    Spacer(Modifier.height(8.dp))
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(9.dp))
            .background(c.Secondary, RoundedCornerShape(9.dp))
            .border(1.dp, c.Border, RoundedCornerShape(9.dp))
            .padding(horizontal = 12.dp, vertical = 8.dp)
            .testTag("connection-banner"),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StatusDot(c.StatusAttention, size = 7)
        Spacer(Modifier.width(9.dp))
        Text(text, color = c.MutedForeground, fontSize = 12.sp, lineHeight = 17.sp)
    }
}

/** The agent's live "working…" row — animated dots driven by the FFI activity state (C12). */
@Composable
private fun WorkingRow(text: String?) {
    val c = LocalHirselColors.current
    val transition = rememberInfiniteTransition(label = "working")
    Row(
        modifier = Modifier
            .clip(RoundedCornerShape(14.dp))
            .background(c.Secondary, RoundedCornerShape(14.dp))
            .padding(horizontal = 12.dp, vertical = 9.dp)
            .semantics { contentDescription = "Agent is working" }
            .testTag("agent-working"),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        repeat(3) { i ->
            val a by transition.animateFloat(
                initialValue = 0.25f,
                targetValue = 1f,
                animationSpec = infiniteRepeatable(
                    animation = tween(600, delayMillis = i * 180),
                    repeatMode = RepeatMode.Reverse,
                ),
                label = "dot$i",
            )
            if (i > 0) Spacer(Modifier.width(4.dp))
            Box(
                modifier = Modifier
                    .size(6.dp)
                    .alpha(a)
                    .background(c.MutedForeground, RoundedCornerShape(9999.dp)),
            )
        }
        Spacer(Modifier.width(9.dp))
        Text(
            text?.takeIf { it.isNotBlank() } ?: "working…",
            color = c.MutedForeground,
            fontSize = 13.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun MessageRow(message: ChatMessage) {
    val c = LocalHirselColors.current
    val owner = message.author == ChatAuthor.OWNER
    val time = shortTime(message.timestamp)
    val metaColor = if (owner) c.OnAccent.copy(alpha = 0.72f) else c.MutedForeground
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = if (owner) Arrangement.End else Arrangement.Start,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth(0.80f)
                .background(if (owner) c.Accent else c.Secondary, RoundedCornerShape(14.dp))
                .padding(horizontal = 12.dp, vertical = 8.dp),
        ) {
            // Agent tool activity — a collapsed summary that expands per-tool (D8).
            if (!owner && message.toolCalls.isNotEmpty()) {
                ToolCallsSummary(message.toolCalls)
                if (message.body.isNotBlank()) Spacer(Modifier.height(6.dp))
            }
            if (message.body.isNotBlank()) {
                Text(
                    message.body,
                    color = if (owner) c.OnAccent else c.Foreground,
                    fontSize = 14.sp,
                    lineHeight = 21.sp,
                )
            }
            // Attachments — thumbnails at a phone-appropriate fidelity (D8).
            if (message.attachments.isNotEmpty()) {
                Spacer(Modifier.height(6.dp))
                AttachmentStrip(message.attachments)
            }
            val footer = when {
                owner && message.error != null -> "Not sent"
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

/** A message that never left the device: a danger-tinted bubble with a retry (C30). */
@Composable
private fun FailedMessageRow(failed: FailedSend, onRetry: () -> Unit) {
    val c = LocalHirselColors.current
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.End,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth(0.80f)
                .background(c.Accent.copy(alpha = 0.35f), RoundedCornerShape(14.dp))
                .border(1.dp, c.StatusDanger.copy(alpha = 0.5f), RoundedCornerShape(14.dp))
                .padding(horizontal = 12.dp, vertical = 8.dp)
                .testTag("failed-message"),
        ) {
            Text(failed.body, color = c.Foreground, fontSize = 14.sp, lineHeight = 21.sp)
            Spacer(Modifier.height(4.dp))
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.align(Alignment.End)) {
                Text("Not sent", color = c.StatusDanger, fontSize = 11.sp, fontWeight = FontWeight.Medium)
                Spacer(Modifier.width(10.dp))
                Text(
                    "Retry",
                    color = c.AccentRing,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .clip(RoundedCornerShape(6.dp))
                        .clickable(onClick = onRetry)
                        .semantics { role = Role.Button; contentDescription = "Retry sending" }
                        .padding(horizontal = 8.dp, vertical = 4.dp)
                        .testTag("retry-send"),
                )
            }
        }
    }
}

/** Collapsed agent tool summary ("Ran N tools") that expands to per-tool lines (D8). */
@Composable
private fun ToolCallsSummary(tools: List<ToolCall>) {
    val c = LocalHirselColors.current
    var expanded by remember { mutableStateOf(false) }
    val label = if (tools.size == 1) "Ran 1 tool" else "Ran ${tools.size} tools"
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(9.dp))
            .background(c.SurfaceRaised, RoundedCornerShape(9.dp))
            .clickable { expanded = !expanded }
            .padding(horizontal = 10.dp, vertical = 7.dp)
            .semantics { role = Role.Button; contentDescription = "$label, tap to ${if (expanded) "collapse" else "expand"}" }
            .testTag("tool-summary"),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("⚙", color = c.MutedForeground, fontSize = 12.sp)
            Spacer(Modifier.width(7.dp))
            Text(label, color = c.MutedForeground, fontSize = 12.sp, fontWeight = FontWeight.Medium, modifier = Modifier.weight(1f))
            Text(if (expanded) "▾" else "▸", color = c.MutedForeground, fontSize = 11.sp)
        }
        if (expanded) {
            tools.forEach { tool ->
                Spacer(Modifier.height(6.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    StatusDot(if (tool.ok) c.StatusSuccess else c.StatusDanger, size = 6)
                    Spacer(Modifier.width(8.dp))
                    Text(
                        tool.name,
                        color = c.Foreground,
                        fontSize = 12.sp,
                        fontFamily = HirselMono,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

/**
 * Attachment thumbnails. The FFI carries [Blob] metadata (id/name/mime/size) but
 * exposes no path to fetch the bytes, so these render as labelled thumbnail tiles
 * rather than live previews. See report: needs a blob-fetch/signed-URL FFI (D9).
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun AttachmentStrip(blobs: List<Blob>) {
    FlowRow(
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
        modifier = Modifier.testTag("attachments"),
    ) {
        blobs.forEach { AttachmentTile(it) }
    }
}

@Composable
private fun AttachmentTile(blob: Blob) {
    val c = LocalHirselColors.current
    val isImage = blob.mime.startsWith("image/")
    Column(
        modifier = Modifier
            .width(96.dp)
            .clip(RoundedCornerShape(10.dp))
            .background(c.Card, RoundedCornerShape(10.dp))
            .border(1.dp, c.Border, RoundedCornerShape(10.dp))
            .padding(8.dp),
    ) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .height(60.dp)
                .clip(RoundedCornerShape(6.dp))
                .background(c.SurfaceRaised, RoundedCornerShape(6.dp)),
            contentAlignment = Alignment.Center,
        ) {
            Text(if (isImage) "🖼" else "📄", fontSize = 22.sp)
        }
        Spacer(Modifier.height(6.dp))
        Text(
            blob.name,
            color = c.Foreground,
            fontSize = 11.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        Text(formatSize(blob.size), color = c.MutedForeground, fontSize = 10.sp)
    }
}

private fun formatSize(bytes: ULong): String {
    val b = bytes.toDouble()
    return when {
        b >= 1_000_000 -> "%.1f MB".format(b / 1_000_000)
        b >= 1_000 -> "%.0f KB".format(b / 1_000)
        else -> "$bytes B"
    }
}

@Composable
private fun ReplyChip(label: String, onClick: () -> Unit) {
    val c = LocalHirselColors.current
    Text(
        label,
        color = c.Foreground,
        fontSize = 12.sp,
        fontWeight = FontWeight.Medium,
        maxLines = 1,
        modifier = Modifier
            .clip(RoundedCornerShape(9999.dp))
            .background(c.Secondary, RoundedCornerShape(9999.dp))
            .border(1.dp, c.Border, RoundedCornerShape(9999.dp))
            .clickable(onClick = onClick)
            .semantics { role = Role.Button; contentDescription = "Reply: $label" }
            .padding(horizontal = 12.dp, vertical = 7.dp)
            .testTag("quick-reply"),
    )
}

/** Best-effort short HH:mm from an RFC3339/ISO timestamp; falls back to the raw
 *  string when it is already short, or null when there is nothing legible. */
internal fun shortTime(raw: String): String? {
    if (raw.isBlank()) return null
    Regex("""T(\d{2}:\d{2})""").find(raw)?.let { return it.groupValues[1] }
    Regex("""^(\d{1,2}:\d{2})""").find(raw)?.let { return it.groupValues[1] }
    return raw.takeIf { it.length <= 8 }
}
