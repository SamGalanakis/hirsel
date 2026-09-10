package dev.hirsel.android.pairing

import android.os.Handler
import android.os.Looper
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import dev.hirsel.core.Client
import dev.hirsel.core.ClientObserver
import dev.hirsel.core.ClientSnapshot
import dev.hirsel.core.ConnectionState
import dev.hirsel.core.LifecycleEvent
import dev.hirsel.core.SendReceipt
import dev.hirsel.core.ThreadRelatedTarget

/** How a hirsel iroh connection should be established. */
sealed interface ConnectionSpec {
    /** Redeem a one-time pairing code; the host issues a device token on success. */
    data class Pairing(
        val ticket: String,
        val code: String,
        val label: String,
        val irohSecretKey: String,
    ) : ConnectionSpec

    /** Reconnect with a previously issued, NodeId-pinned device token. */
    data class Device(val credential: DeviceCredential) : ConnectionSpec
}

/** A legible connection phase for the UI, derived from client-core lifecycle events. */
sealed interface Phase {
    data object Connecting : Phase
    data class Reconnecting(val attempt: Int) : Phase
    data object Online : Phase
    data class Offline(val reason: String?) : Phase
    data class Failed(val detail: String) : Phase
}

/** An outbound message that never reached the host, kept so the UI can offer a retry. */
data class FailedSend(val id: Long, val body: String, val threadId: ULong, val artifactIds: List<ULong>, val historyId: String)
data class ActionFailure(val detail: String, val historyId: String? = null, val threadId: ULong? = null, val clientId: String? = null)
internal fun ActionFailure.visibleIn(historyId: String?, threadId: ULong?): Boolean =
    (this.historyId == null || this.historyId == historyId) && (this.threadId == null || this.threadId == threadId)

internal enum class PendingRequestKind { Action, Create, Open, Related }
private data class PendingRequest(val kind: PendingRequestKind, val historyId: String?, val threadId: ULong?)
private const val REQUEST_TIMEOUT_MS = 20_000L

internal class PendingRequests {
    private val entries = mutableMapOf<String, PendingRequest>()

    fun track(clientId: String, kind: PendingRequestKind, historyId: String?, threadId: ULong?) {
        entries[clientId] = PendingRequest(kind, historyId, threadId)
    }

    fun clear() {
        entries.clear()
    }

    fun accept(clientId: String, kind: PendingRequestKind, historyId: String? = null, threadId: ULong? = null): Boolean {
        val pending = entries[clientId]
        if (pending?.kind != kind || historyId != null && pending.historyId != historyId || threadId != null && pending.threadId != threadId) return false
        entries.remove(clientId)
        return true
    }

    fun fail(detail: String, clientId: String?): ActionFailure? {
        if (clientId == null) return ActionFailure(detail)
        val pending = entries.remove(clientId) ?: return null
        return ActionFailure(detail, pending.historyId, pending.threadId, clientId)
    }

    fun timeout(clientId: String): ActionFailure? {
        val pending = entries.remove(clientId) ?: return null
        return ActionFailure("Thread request timed out", pending.historyId, pending.threadId, clientId)
    }
}

/**
 * Live, observable state of a single native [Client]. Callbacks arrive on native
 * threads and are marshalled to the main thread before touching Compose state.
 */
class Connection internal constructor(
    val client: Client?,
    private val mainHandler: Handler,
) {
    var snapshot by mutableStateOf<ClientSnapshot?>(null)
        internal set
    var phase by mutableStateOf<Phase>(if (client == null) Phase.Failed("Invalid pairing link.") else Phase.Connecting)
        internal set

    /**
     * Sends that threw before the host accepted them. The happy path is owned by
     * native: `sendThreadMessage` inserts an optimistic message (keyed by its
     * `clientId`) with `pending = true`, then the host's ack flips it to sent via
     * a fresh snapshot — that reconciliation needs no bookkeeping here. Only the
     * failure case surfaces, as a retryable bubble.
     */
    val failedSends = mutableStateListOf<FailedSend>()
    private var failCounter = 0L

    var focusedThreadId by mutableStateOf<ULong?>(null)
    val drafts = mutableStateMapOf<ULong, String>()
    val recoveredDrafts = mutableStateListOf<String>()
    var creatingClientId by mutableStateOf<String?>(null)
    var actionError by mutableStateOf<ActionFailure?>(null)
    private val pendingRequests = PendingRequests()

    fun openThread(id: ULong, beforeId: ULong? = null) {
        focusedThreadId = id
        val receipt = client?.openThread(id, beforeId) ?: return
        trackRequest(receipt, PendingRequestKind.Open, snapshot?.historyId, id)
    }

    fun createThread(historyId: String, title: String, parentThreadId: ULong?) {
        val receipt = client?.createThread(historyId, title, parentThreadId)
        creatingClientId = receipt?.clientId
        if (receipt != null) trackRequest(receipt, PendingRequestKind.Create, historyId, parentThreadId)
    }

    fun action(historyId: String, threadId: ULong, action: String, data: String = "{}", revision: ULong? = null) {
        val receipt = runCatching { client?.threadAction(historyId, threadId, action, data, revision) }
            .getOrElse {
                actionError = ActionFailure(it.message ?: "Action failed", historyId, threadId)
                return
            }
        if (receipt == null) {
            actionError = ActionFailure("Thread or history changed. Reopen this control and try again.", historyId, threadId)
            return
        }
        trackRequest(receipt, PendingRequestKind.Action, historyId, threadId)
    }

    fun updateThreadIcon(historyId: String, threadId: ULong, icon: String?, revision: ULong): Boolean {
        val receipt = client?.updateThreadIcon(historyId, threadId, icon, revision) ?: return false
        trackRequest(receipt, PendingRequestKind.Action, historyId, threadId)
        return true
    }

    private fun trackRequest(receipt: SendReceipt, kind: PendingRequestKind, historyId: String?, threadId: ULong?) {
        pendingRequests.track(receipt.clientId, kind, historyId, threadId)
        mainHandler.postDelayed({
            pendingRequests.timeout(receipt.clientId)?.let {
                if (creatingClientId == receipt.clientId) creatingClientId = null
                actionError = it
            }
        }, REQUEST_TIMEOUT_MS)
    }

    internal fun clearPendingRequests() {
        pendingRequests.clear()
    }

    internal fun acceptAction(clientId: String, historyId: String, threadId: ULong) {
        if (!pendingRequests.accept(clientId, PendingRequestKind.Action, historyId, threadId)) return
        if (actionError?.clientId == clientId) actionError = null
    }

    internal fun acceptCreated(clientId: String) {
        if (!pendingRequests.accept(clientId, PendingRequestKind.Create)) return
        if (actionError?.clientId == clientId) actionError = null
    }

    internal fun acceptOpened(clientId: String, threadId: ULong) {
        if (!pendingRequests.accept(clientId, PendingRequestKind.Open, threadId = threadId)) return
        if (actionError?.clientId == clientId) actionError = null
    }

    internal fun acceptRelated(clientId: String, historyId: String, threadId: ULong) {
        if (!pendingRequests.accept(clientId, PendingRequestKind.Related, historyId, threadId)) return
        if (actionError?.clientId == clientId) actionError = null
    }

    internal fun receiveProtocolError(detail: String, clientId: String?) {
        pendingRequests.fail(detail, clientId)?.let {
            if (creatingClientId == clientId) creatingClientId = null
            actionError = it
        }
    }

    // Callers capture historyId with the addressed Thread, before any delayed UI action.
    fun addThreadRelated(historyId: String, threadId: ULong, target: ThreadRelatedTarget, title: String?): SendReceipt? =
        client?.addThreadRelated(historyId, threadId, target, title)?.also {
            trackRequest(it, PendingRequestKind.Related, historyId, threadId)
        }

    fun removeThreadRelated(historyId: String, threadId: ULong, itemId: ULong): SendReceipt? =
        client?.removeThreadRelated(historyId, threadId, itemId)?.also {
            trackRequest(it, PendingRequestKind.Related, historyId, threadId)
        }

    fun stop(historyId: String, threadId: ULong) { client?.cancelTurn(historyId, threadId) }

    val isOnline: Boolean get() = phase is Phase.Online

    /** Queue locally; a throw becomes a retryable [FailedSend]. */
    fun send(body: String, threadId: ULong, expectedHistoryId: String?, artifactIds: List<ULong> = emptyList()) {
        if (expectedHistoryId == null || expectedHistoryId != snapshot?.historyId) {
            recoveredDrafts.add(body)
            actionError = ActionFailure("History changed. Restore this text to a current Thread before sending.", expectedHistoryId, threadId)
            return
        }
        val c = client ?: run { recordFailure(body, threadId, artifactIds, expectedHistoryId); return }
        // This native method only queues locally; keep it ordered with identity callbacks.
        runCatching { c.sendThreadMessage(expectedHistoryId, threadId, body, emptyList(), emptyList(), artifactIds.toList()) }
            .onFailure { recordFailure(body, threadId, artifactIds, expectedHistoryId) }
    }

    /** Drop the failed entry and try the same body again. */
    fun retry(failed: FailedSend) {
        failedSends.remove(failed)
        send(failed.body, failed.threadId, failed.historyId, failed.artifactIds)
    }

    private fun recordFailure(body: String, threadId: ULong, artifactIds: List<ULong>, historyId: String) {
        failedSends.add(FailedSend(failCounter++, body, threadId, artifactIds.toList(), historyId))
    }

    /** The device token the host issued during a successful pairing handshake. */
    fun issuedDeviceToken(): String? = runCatching { client?.issuedDeviceToken() }.getOrNull()
}

/**
 * Builds a [Client] for [spec], connects it off the main thread, and streams its
 * snapshots + lifecycle into an observable [Connection]. The client is
 * disconnected and freed when [spec] changes or the composition leaves.
 */
@Composable
fun rememberConnection(spec: ConnectionSpec): Connection {
    val mainHandler = remember { Handler(Looper.getMainLooper()) }
    val connection = remember(spec) { openConnection(spec, mainHandler) }

    DisposableEffect(spec) {
        val client = connection.client
        if (client != null) {
            Thread {
                runCatching { client.connect() }
                    .onFailure { error ->
                        mainHandler.post {
                            connection.phase = Phase.Failed(error.message ?: "Connection failed.")
                        }
                    }
            }.start()
        }
        onDispose {
            if (client != null) {
                Thread {
                    runCatching { client.disconnect() }
                    runCatching { client.close() }
                }.start()
            }
        }
    }
    return connection
}

private fun openConnection(spec: ConnectionSpec, mainHandler: Handler): Connection {
    // The observer needs a reference to the Connection, but the Connection needs
    // the client the observer is wired into. A one-shot holder closes the loop:
    // the observer reads `target.value`, which we set once the Connection exists.
    val target = ConnectionRef()
    val observer = object : ClientObserver {
        override fun onStateChanged(snapshot: ClientSnapshot) {
            mainHandler.post {
                val conn = target.value ?: return@post
                val old = conn.snapshot
                if (old?.historyId != null && old.historyId != snapshot.historyId) {
                    conn.recoveredDrafts.addAll(conn.drafts.values.filter { it.isNotBlank() })
                    conn.recoveredDrafts.addAll(conn.failedSends.map { it.body })
                    conn.drafts.clear()
                    conn.failedSends.clear()
                    conn.focusedThreadId = null
                    conn.creatingClientId = null
                    conn.clearPendingRequests()
                    conn.actionError = ActionFailure("History changed. Unsent text is available in recovered drafts.")
                }
                val priorRecovered = old?.recoveredDrafts.orEmpty().toSet()
                conn.recoveredDrafts.addAll(snapshot.recoveredDrafts.filter { it !in priorRecovered })
                conn.snapshot = snapshot
                snapshot.createdThreads.forEach { conn.acceptCreated(it.clientId) }
                snapshot.createdThreads.firstOrNull { it.clientId == conn.creatingClientId }?.let {
                    conn.creatingClientId = null
                    conn.openThread(it.threadId)
                }
                if (snapshot.connection == ConnectionState.ONLINE && conn.phase !is Phase.Online) {
                    conn.phase = Phase.Online
                }
            }
        }

        override fun onLifecycleEvent(event: LifecycleEvent) {
            mainHandler.post {
                val conn = target.value ?: return@post
                conn.phase = when (event) {
                    is LifecycleEvent.Connecting ->
                        if (event.attempt == 0u) Phase.Connecting else Phase.Reconnecting(event.attempt.toInt() + 1)
                    is LifecycleEvent.Online -> Phase.Online
                    is LifecycleEvent.Offline -> Phase.Offline(event.reason)
                    is LifecycleEvent.ThreadOpened -> {
                        conn.acceptOpened(event.clientId, event.threadId)
                        conn.phase
                    }
                    is LifecycleEvent.ThreadRelatedChanged -> {
                        event.clientId?.let { conn.acceptRelated(it, event.historyId, event.threadId) }
                        conn.phase
                    }
                    is LifecycleEvent.ThreadActionApplied -> {
                        conn.acceptAction(event.clientId, event.historyId, event.threadId)
                        conn.phase
                    }
                    is LifecycleEvent.ProtocolError -> {
                        conn.receiveProtocolError(event.detail, event.clientId)
                        conn.phase
                    }
                }
            }
        }
    }
    val client = runCatching {
        when (spec) {
            is ConnectionSpec.Pairing ->
                Client.newIrohPairing(
                    spec.ticket,
                    spec.code,
                    spec.label,
                    spec.irohSecretKey,
                    observer,
                )
            is ConnectionSpec.Device ->
                Client.newIroh(
                    spec.credential.ticket,
                    spec.credential.deviceToken,
                    spec.credential.irohSecretKey,
                    observer,
                )
        }
    }.getOrNull()
    return Connection(client, mainHandler).also { target.value = it }
}

private class ConnectionRef {
    @Volatile
    var value: Connection? = null
}
