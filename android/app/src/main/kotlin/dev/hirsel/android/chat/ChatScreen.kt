package dev.hirsel.android.chat

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
import dev.hirsel.android.ui.ErrorCopy
import dev.hirsel.android.ui.HirselMono
import dev.hirsel.android.ui.LocalHirselColors
import dev.hirsel.core.AgentActivityState
import dev.hirsel.core.Blob
import dev.hirsel.core.ChatAuthor
import dev.hirsel.core.ChatMessage
import dev.hirsel.core.Ping
import dev.hirsel.core.PingStatus
import dev.hirsel.core.ToolCall
import kotlinx.coroutines.launch

/**
 * The primary chat surface. Renders the conversation (messages, agent tool
 * activity, attachments), the live agent-working indicator, ping cards with
 * quick-reply affordances, a connection banner with friendly copy, and the
 * composer — auto-scrolling to the newest content unless the user has scrolled up.
 */
@Composable
fun ChatScreen(
    connection: Connection,
    onOpenSettings: () -> Unit,
) {
    val snapshot = connection.snapshot
    val phase = connection.phase
    val messages = snapshot?.messages.orEmpty()
    val pings = snapshot?.pings.orEmpty()
    val activity = snapshot?.agentActivity
    val thinking = activity?.state == AgentActivityState.THINKING
    val failed = connection.failedSends
    val c = LocalHirselColors.current

    var draft by remember { mutableStateOf("") }
    val send = {
        draft.trim().takeIf(String::isNotEmpty)?.let {
            connection.send(it)
            draft = ""
        }
        Unit
    }

    val listState = rememberLazyListState()
    val scope = rememberCoroutineScope()
    // "Pinned to newest": the last item is (nearly) visible. When true we follow
    // new content; when the user scrolls up to read history we leave them be.
    val atBottom by remember {
        derivedStateOf {
            val info = listState.layoutInfo
            val last = info.visibleItemsInfo.lastOrNull() ?: return@derivedStateOf true
            last.index >= info.totalItemsCount - 1
        }
    }
    // Auto-scroll to newest when new content lands and we're already at the bottom.
    LaunchedEffect(messages.size, failed.size, pings.size, thinking) {
        if (atBottom) {
            val target = listState.layoutInfo.totalItemsCount - 1
            if (target >= 0) listState.animateScrollToItem(target)
        }
    }

    val banner: String? = when (val p = phase) {
        is Phase.Reconnecting -> "Reconnecting to your host…"
        is Phase.Offline -> ErrorCopy.connection(p.reason)
        is Phase.Failed -> ErrorCopy.connection(p.detail)
        else -> null
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .statusBarsPadding()
            .navigationBarsPadding()
            .imePadding()
            .padding(horizontal = 16.dp, vertical = 12.dp),
    ) {
        // Thin top bar: wordmark left, connection pill + gear right.
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("hirsel", fontSize = 16.sp, fontWeight = FontWeight.SemiBold, color = c.Foreground)
            Spacer(Modifier.weight(1f))
            ConnectionPill(phase)
            Spacer(Modifier.width(6.dp))
            GearButton(onClick = onOpenSettings)
        }

        Spacer(Modifier.height(10.dp))
        HairlineDivider()

        if (banner != null) {
            ConnectionBanner(banner)
        }
        Spacer(Modifier.height(10.dp))

        val hasContent = messages.isNotEmpty() || pings.isNotEmpty() || failed.isNotEmpty()
        Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
            if (!hasContent) {
                val connecting = snapshot == null || phase is Phase.Connecting || phase is Phase.Reconnecting
                ChatPlaceholder(connecting = connecting)
            } else {
                LazyColumn(
                    state = listState,
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

                    items(failed, key = { "failed-${it.id}" }) { f ->
                        Spacer(Modifier.height(6.dp))
                        FailedMessageRow(f, onRetry = { connection.retry(f) })
                    }

                    if (thinking) {
                        item(key = "agent-activity") {
                            Spacer(Modifier.height(10.dp))
                            WorkingRow(activity?.text)
                        }
                    }

                    if (pings.isNotEmpty()) {
                        item(key = "pings-header") {
                            Spacer(Modifier.height(if (messages.isEmpty()) 0.dp else 20.dp))
                            Text(
                                "Pings",
                                style = microLabel(),
                                color = c.MutedForeground,
                                modifier = Modifier.padding(bottom = 8.dp),
                            )
                        }
                        itemsIndexed(pings, key = { _, p -> "ping-${p.id}" }) { index, ping ->
                            if (index > 0) Spacer(Modifier.height(8.dp))
                            PingCard(ping, onReply = { connection.send(it) })
                        }
                    }
                    item(key = "tail-spacer") { Spacer(Modifier.height(4.dp)) }
                }

                // Jump-to-latest — only when the user has scrolled up off the bottom.
                if (!atBottom) {
                    JumpToLatest(
                        modifier = Modifier
                            .align(Alignment.BottomCenter)
                            .padding(bottom = 8.dp),
                        onClick = {
                            scope.launch {
                                val target = listState.layoutInfo.totalItemsCount - 1
                                if (target >= 0) listState.animateScrollToItem(target)
                            }
                        },
                    )
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
                    containerColor = c.Accent,
                    contentColor = c.OnAccent,
                    disabledContainerColor = c.Secondary,
                    disabledContentColor = c.MutedForeground,
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
private fun JumpToLatest(modifier: Modifier = Modifier, onClick: () -> Unit) {
    val c = LocalHirselColors.current
    Row(
        modifier = modifier
            .clip(RoundedCornerShape(9999.dp))
            .background(c.Accent, RoundedCornerShape(9999.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 14.dp, vertical = 7.dp)
            .testTag("jump-to-latest")
            .semantics { contentDescription = "Jump to latest" },
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("↓ Latest", color = c.OnAccent, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
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
private fun ChatPlaceholder(connecting: Boolean) {
    val c = LocalHirselColors.current
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        if (connecting) {
            CircularProgressIndicator(color = c.AccentRing, strokeWidth = 2.5.dp, modifier = Modifier.size(26.dp))
            Spacer(Modifier.height(16.dp))
            Text("Connecting over iroh…", color = c.MutedForeground, fontSize = 13.sp)
        } else {
            Text("No messages yet", fontWeight = FontWeight.SemiBold, fontSize = 15.sp, color = c.Foreground)
            Spacer(Modifier.height(6.dp))
            Text(
                "Send a message to your agent — it's listening.",
                color = c.MutedForeground,
                fontSize = 13.sp,
                lineHeight = 20.sp,
                modifier = Modifier.widthIn(max = 280.dp),
            )
        }
    }
}

/**
 * A ping card. Tappable to expand its full content and mark it read (C31);
 * a ping that requires a response shows quick-reply chips and an inline reply
 * field that send back through the FFI message path (C4).
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun PingCard(ping: Ping, onReply: (String) -> Unit) {
    val c = LocalHirselColors.current
    val done = ping.status == PingStatus.DONE
    val requires = ping.requiresResponse && !done

    var expanded by remember(ping.id) { mutableStateOf(false) }
    // Local read state: the FFI exposes no mark-read call, so tapping marks the
    // ping read on-device for immediate feedback. See report (needs an FFI
    // read/ack method to persist to the host).
    var locallyRead by remember(ping.id) { mutableStateOf(ping.read) }
    var replyDraft by remember(ping.id) { mutableStateOf("") }

    val hasMore = ping.content.isNotBlank() && ping.content.trim() != ping.description.trim()

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .height(IntrinsicSize.Min)
            .clip(RoundedCornerShape(14.dp))
            .alpha(if (done) 0.6f else 1f)
            .background(c.Card, RoundedCornerShape(14.dp))
            .border(1.dp, c.Border, RoundedCornerShape(14.dp))
            .clickable(enabled = hasMore || !locallyRead) {
                if (hasMore) expanded = !expanded
                locallyRead = true
            }
            .testTag("ping-card"),
    ) {
        Row(modifier = Modifier.height(IntrinsicSize.Min)) {
            Box(
                modifier = Modifier
                    .width(3.dp)
                    .fillMaxHeight()
                    .background(if (requires) c.Accent else Color.Transparent),
            )
            Column(Modifier.padding(horizontal = 13.dp, vertical = 11.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                    if (requires && !locallyRead) {
                        StatusDot(c.Accent, size = 7)
                        Spacer(Modifier.width(7.dp))
                    }
                    Text(
                        "@${ping.name}",
                        color = c.Foreground,
                        fontWeight = FontWeight.SemiBold,
                        fontSize = 13.sp,
                        fontFamily = HirselMono,
                        modifier = Modifier.testTag("ping-name"),
                    )
                    Spacer(Modifier.weight(1f))
                    if (done) {
                        Text("✓", color = c.StatusSuccess, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                        Spacer(Modifier.width(4.dp))
                        Text("Done", style = microLabel(), color = c.MutedForeground)
                    } else {
                        shortTime(ping.timestamp)?.let {
                            Text(it, color = c.MutedForeground, fontSize = 11.sp)
                        }
                    }
                }
                Spacer(Modifier.height(4.dp))
                Text(
                    if (expanded && hasMore) ping.content else ping.description,
                    color = if (requires) c.Foreground else c.MutedForeground,
                    fontWeight = if (requires) FontWeight.Medium else FontWeight.Normal,
                    fontSize = 13.sp,
                    lineHeight = 20.sp,
                )
                if (hasMore) {
                    Spacer(Modifier.height(4.dp))
                    Text(
                        if (expanded) "Show less" else "Show more",
                        color = c.AccentRing,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.Medium,
                    )
                }

                // Quick-reply affordances (C4) — chips + an inline reply field.
                if (requires) {
                    Spacer(Modifier.height(10.dp))
                    if (ping.quickReplies.isNotEmpty()) {
                        FlowRow(
                            horizontalArrangement = Arrangement.spacedBy(6.dp),
                            verticalArrangement = Arrangement.spacedBy(6.dp),
                        ) {
                            ping.quickReplies.forEach { qr ->
                                ReplyChip(qr.label) {
                                    onReply(qr.value)
                                    locallyRead = true
                                }
                            }
                        }
                        Spacer(Modifier.height(8.dp))
                    }
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Box(modifier = Modifier.weight(1f)) {
                            HirselField(
                                value = replyDraft,
                                onValueChange = { replyDraft = it },
                                placeholder = "Reply…",
                                testTag = "ping-reply-field",
                                singleLine = true,
                                imeAction = ImeAction.Send,
                                onImeAction = {
                                    replyDraft.trim().takeIf(String::isNotEmpty)?.let {
                                        onReply(it)
                                        replyDraft = ""
                                        locallyRead = true
                                    }
                                },
                                contentDescription = "Reply to ${ping.name}",
                            )
                        }
                        Spacer(Modifier.width(8.dp))
                        val canSend = replyDraft.isNotBlank()
                        Text(
                            "Send",
                            color = if (canSend) c.AccentRing else c.MutedForeground,
                            fontSize = 13.sp,
                            fontWeight = FontWeight.SemiBold,
                            modifier = Modifier
                                .clip(RoundedCornerShape(6.dp))
                                .clickable(enabled = canSend) {
                                    onReply(replyDraft.trim())
                                    replyDraft = ""
                                    locallyRead = true
                                }
                                .semantics { role = Role.Button; contentDescription = "Send reply" }
                                .padding(horizontal = 8.dp, vertical = 6.dp)
                                .testTag("ping-reply-send"),
                        )
                    }
                }
            }
        }
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
