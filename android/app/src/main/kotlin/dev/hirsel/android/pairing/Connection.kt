package dev.hirsel.android.pairing

import android.os.Handler
import android.os.Looper
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import dev.hirsel.core.Client
import dev.hirsel.core.ClientObserver
import dev.hirsel.core.ClientSnapshot
import dev.hirsel.core.ConnectionState
import dev.hirsel.core.LifecycleEvent

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

/**
 * Live, observable state of a single native [Client]. Callbacks arrive on native
 * threads and are marshalled to the main thread before touching Compose state.
 */
class Connection internal constructor(val client: Client?) {
    var snapshot by mutableStateOf<ClientSnapshot?>(null)
        internal set
    var phase by mutableStateOf<Phase>(if (client == null) Phase.Failed("Invalid pairing link.") else Phase.Connecting)
        internal set

    val isOnline: Boolean get() = phase is Phase.Online

    fun send(body: String) {
        client?.sendMessage(body)
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
                conn.snapshot = snapshot
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
                    is LifecycleEvent.ProtocolError -> Phase.Failed(event.detail)
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
    return Connection(client).also { target.value = it }
}

private class ConnectionRef {
    @Volatile
    var value: Connection? = null
}
