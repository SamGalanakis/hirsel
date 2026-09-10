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

private const val THREAD_CHANNEL_ID = "hirsel-threads"

class HirselFirebaseMessagingService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        if (PushRegistrationStore.get(this).recordToken(token)) {
            Log.i(FCM_LOG_TAG, "FCM token refreshed")
        } else {
            Log.w(FCM_LOG_TAG, "Firebase returned an empty FCM token")
        }
    }

    override fun onMessageReceived(message: RemoteMessage) {
        val data = parseThreadNotificationData(message.data) ?: return
        val title = message.notification?.title ?: "Hirsel"
        val body = message.notification?.body ?: data.name
        if (!PushRegistrationStore.get(this).state.value.enabled) {
            Log.i(FCM_LOG_TAG, "push disabled in settings; suppressing notification for Thread ${data.threadId}")
            return
        }
        postThreadNotification(title, body, data)
    }

    private fun postThreadNotification(title: String, body: String, data: ThreadNotificationData) {
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
            data.intentExtras().forEach { (key, value) -> putExtra(key, value) }
        }
        val pendingIntent = PendingIntent.getActivity(
            this,
            (data.historyId + ":" + data.threadId).hashCode(),
            launchIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(this, THREAD_CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle(title)
            .setContentText(body)
            .setSubText("@${data.name} · Thread #${data.threadId}")
            .setAutoCancel(true)
            .setContentIntent(pendingIntent)
            .build()
        manager.notify(data.threadId.toIntOrNull() ?: (data.historyId + ":" + data.threadId).hashCode(), notification)
    }
}
