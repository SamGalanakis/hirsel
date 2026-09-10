package dev.hirsel.android

/** A numeric Thread ID is meaningful only in the exact history that issued the push. */
internal fun notificationDestination(
    notifiedHistoryId: String?,
    notifiedThreadId: ULong?,
    currentHistoryId: String?,
    availableThreadIds: Collection<ULong>,
): ULong? = notifiedThreadId?.takeIf {
    !notifiedHistoryId.isNullOrBlank() && notifiedHistoryId == currentHistoryId && it in availableThreadIds
}
