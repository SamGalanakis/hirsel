package dev.hirsel.android.pairing

import dev.hirsel.android.notificationDestination
import dev.hirsel.core.ConnectionState
import dev.hirsel.core.ThreadRelatedTarget

/** A Related edge navigates only within the currently connected history. */
internal fun relatedThreadDestination(
    target: ThreadRelatedTarget,
    connection: ConnectionState,
    currentHistoryId: String?,
    availableThreadIds: Collection<ULong>,
): ULong? {
    if (connection != ConnectionState.ONLINE || target !is ThreadRelatedTarget.Thread) return null
    return notificationDestination(target.historyId, target.threadId, currentHistoryId, availableThreadIds)
}
