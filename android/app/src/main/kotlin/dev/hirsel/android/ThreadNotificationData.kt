package dev.hirsel.android

internal data class ThreadNotificationData(
    val name: String,
    val threadId: String,
    val historyId: String,
) {
    fun intentExtras(): Map<String, String> = mapOf(
        "thread_id" to threadId,
        "history_id" to historyId,
    )
}

internal fun parseThreadNotificationData(data: Map<String, String>): ThreadNotificationData? {
    val name = data["title"] ?: return null
    val threadId = data["thread_id"]?.takeIf { it.toULongOrNull() != null } ?: return null
    val historyId = data["history_id"]?.takeIf { it.isNotBlank() } ?: return null
    return ThreadNotificationData(name, threadId, historyId)
}
