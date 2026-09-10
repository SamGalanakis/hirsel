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
import dev.hirsel.android.settings.SettingsStore

const val FCM_LOG_TAG = "HirselFcm"

private const val THREAD_CHANNEL_ID = "hirsel-threads"

class HirselFirebaseMessagingService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        Log.i(FCM_LOG_TAG, "FCM token refreshed")
    }

    override fun onMessageReceived(message: RemoteMessage) {
        val name = message.data["title"] ?: return
        val threadId = message.data["thread_id"]?.takeIf { it.toULongOrNull() != null } ?: return
        val historyId = message.data["history_id"]?.takeIf { it.isNotBlank() } ?: return
        val title = message.notification?.title ?: "Hirsel"
        val body = message.notification?.body ?: name
        if (!SettingsStore(this).pushEnabled) {
            Log.i(FCM_LOG_TAG, "push disabled in settings; suppressing notification for Thread $threadId")
            return
        }
        postThreadNotification(title, body, name, threadId, historyId)
    }

    private fun postThreadNotification(title: String, body: String, name: String, threadId: String, historyId: String) {
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager.createNotificationChannel(
                NotificationChannel(
                    THREAD_CHANNEL_ID,
                    "Hirsel threads",
                    NotificationManager.IMPORTANCE_HIGH,
                ),
            )
        }

        val launchIntent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP
            putExtra("thread_id", threadId)
            putExtra("history_id", historyId)
        }
        val pendingIntent = PendingIntent.getActivity(
            this,
            (historyId + ":" + threadId).hashCode(),
            launchIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(this, THREAD_CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle(title)
            .setContentText(body)
            .setSubText("@$name · Thread #$threadId")
            .setAutoCancel(true)
            .setContentIntent(pendingIntent)
            .build()
        manager.notify(threadId.toIntOrNull() ?: (historyId + ":" + threadId).hashCode(), notification)
    }
}
