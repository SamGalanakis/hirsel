package dev.hirsel.android

import android.os.Bundle
import android.os.Handler
import android.os.Looper
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import dev.hirsel.core.ChatAuthor
import dev.hirsel.core.ChatMessage
import dev.hirsel.core.Client
import dev.hirsel.core.ClientObserver
import dev.hirsel.core.ClientSnapshot
import dev.hirsel.core.ConnectionState
import dev.hirsel.core.LifecycleEvent
import dev.hirsel.core.Ping

private const val HOST = "http://10.0.2.2:3090"
private const val TOKEN = "dev-token"

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                HirselApp()
            }
        }
    }
}

@Composable
private fun HirselApp() {
    var clientSnapshot by remember { mutableStateOf<ClientSnapshot?>(null) }
    var lifecycle by remember { mutableStateOf("Starting") }
    val mainHandler = remember { Handler(Looper.getMainLooper()) }
    val observer = remember {
        object : ClientObserver {
            override fun onStateChanged(snapshot: ClientSnapshot) {
                mainHandler.post { clientSnapshot = snapshot }
            }

            override fun onLifecycleEvent(event: LifecycleEvent) {
                val label = when (event) {
                    is LifecycleEvent.Connecting -> "Connecting (${event.attempt + 1u})"
                    is LifecycleEvent.Online -> "Online"
                    is LifecycleEvent.Offline -> event.reason?.let { "Offline: $it" } ?: "Offline"
                    is LifecycleEvent.ProtocolError -> "Protocol error: ${event.detail}"
                }
                mainHandler.post { lifecycle = label }
            }
        }
    }
    val client = remember { Client(HOST, TOKEN, observer) }

    LaunchedEffect(client) {
        runCatching { withContext(Dispatchers.IO) { client.connect() } }
            .onFailure { lifecycle = "Connection error: ${it.message}" }
    }
    DisposableEffect(client) {
        onDispose {
            Thread {
                runCatching { client.disconnect() }
                client.close()
            }.start()
        }
    }

    Surface(
        modifier = Modifier
            .fillMaxSize()
            .statusBarsPadding()
            .semantics { testTagsAsResourceId = true },
        color = Color(0xFFF7F7F2),
    ) {
        ChatScreen(
            snapshot = clientSnapshot,
            lifecycle = lifecycle,
            onSend = client::sendMessage,
        )
    }
}

@Composable
private fun ChatScreen(
    snapshot: ClientSnapshot?,
    lifecycle: String,
    onSend: (String) -> Unit,
) {
    var draft by remember { mutableStateOf("") }
    val pings = snapshot?.pings.orEmpty()
    val send = {
        draft.trim().takeIf(String::isNotEmpty)?.let {
            onSend(it)
            draft = ""
        }
        Unit
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(horizontal = 16.dp, vertical = 12.dp),
    ) {
        Text("Hirsel", fontSize = 28.sp, fontWeight = FontWeight.Bold)
        Spacer(Modifier.height(8.dp))
        ConfigCard(snapshot?.connection, lifecycle)
        Spacer(Modifier.height(10.dp))

        LazyColumn(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .testTag("chat-list"),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            item {
                Text("Chat", fontSize = 18.sp, fontWeight = FontWeight.SemiBold)
            }
            if (snapshot == null) {
                item { Text("Waiting for the shared Rust client…") }
            } else if (snapshot.messages.isEmpty()) {
                item { Text("No messages yet") }
            } else {
                items(
                    snapshot.messages,
                    key = { "message-${it.id?.toString() ?: it.clientId.orEmpty()}" },
                ) {
                    MessageRow(it)
                }
            }

            item {
                Spacer(Modifier.height(4.dp))
                Text("Pings", fontSize = 18.sp, fontWeight = FontWeight.SemiBold)
            }
            if (pings.isEmpty()) {
                item { Text("No pings") }
            } else {
                items(pings, key = { "ping-${it.id}" }) { PingRow(it) }
            }
        }

        Spacer(Modifier.height(10.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            OutlinedTextField(
                value = draft,
                onValueChange = { draft = it },
                modifier = Modifier
                    .weight(1f)
                    .testTag("message-composer")
                    .semantics { contentDescription = "Message composer" },
                label = { Text("Message") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                keyboardActions = KeyboardActions(onSend = { send() }),
            )
            Spacer(Modifier.width(8.dp))
            Button(
                onClick = send,
                enabled = draft.isNotBlank(),
                modifier = Modifier
                    .testTag("send-message")
                    .semantics { contentDescription = "Send message" },
            ) {
                Text("Send")
            }
        }
    }
}

@Composable
private fun ConfigCard(connection: ConnectionState?, lifecycle: String) {
    Card(
        colors = CardDefaults.cardColors(containerColor = Color(0xFFE9EFE7)),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Connection", fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.width(8.dp))
                Text(
                    text = connection?.name ?: lifecycle,
                    color = if (connection == ConnectionState.ONLINE) Color(0xFF176B3A) else Color(0xFF71550A),
                    modifier = Modifier
                        .background(Color.White, RoundedCornerShape(12.dp))
                        .padding(horizontal = 8.dp, vertical = 2.dp),
                )
            }
            Text("Host  $HOST", fontSize = 12.sp)
            Text("Token  $TOKEN", fontSize = 12.sp)
        }
    }
}

@Composable
private fun MessageRow(message: ChatMessage) {
    val owner = message.author == ChatAuthor.OWNER
    Card(
        colors = CardDefaults.cardColors(
            containerColor = if (owner) Color(0xFFDCEBFF) else Color.White,
        ),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) {
            Text(
                if (owner) "You${if (message.pending) " · sending" else ""}" else "Agent",
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                color = Color(0xFF475569),
            )
            Text(message.body)
        }
    }
}

@Composable
private fun PingRow(ping: Ping) {
    Card(
        colors = CardDefaults.cardColors(containerColor = Color(0xFFFFF2C7)),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) {
            Text("@${ping.name}", fontWeight = FontWeight.Bold)
            Text(ping.description, fontSize = 13.sp)
        }
    }
}
