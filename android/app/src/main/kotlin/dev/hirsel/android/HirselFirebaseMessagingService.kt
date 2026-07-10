package dev.hirsel.android

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log
import androidx.core.app.NotificationCompat
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

const val FCM_LOG_TAG = "HirselFcm"

private const val PING_CHANNEL_ID = "hirsel-pings"

class HirselFirebaseMessagingService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        Log.i(FCM_LOG_TAG, "FCM token refreshed: ${token.take(16)}…")
    }

    override fun onMessageReceived(message: RemoteMessage) {
        val name = message.data["name"] ?: "ping"
        val pingId = message.data["ping_id"] ?: "unknown"
        val title = message.notification?.title ?: "Hirsel"
        val body = message.notification?.body ?: name
        Log.i(
            FCM_LOG_TAG,
            "FCM message received: name=$name ping_id=$pingId title=$title body=$body data=${message.data}",
        )
        postPingNotification(title, body, name, pingId)
    }

    private fun postPingNotification(title: String, body: String, name: String, pingId: String) {
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager.createNotificationChannel(
                NotificationChannel(
                    PING_CHANNEL_ID,
                    "Hirsel pings",
                    NotificationManager.IMPORTANCE_HIGH,
                ),
            )
        }

        val launchIntent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP
        }
        val pendingIntent = PendingIntent.getActivity(
            this,
            pingId.hashCode(),
            launchIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(this, PING_CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle(title)
            .setContentText(body)
            .setSubText("@$name · Ping $pingId")
            .setAutoCancel(true)
            .setContentIntent(pendingIntent)
            .build()
        manager.notify(pingId.toIntOrNull() ?: pingId.hashCode(), notification)
    }
}
