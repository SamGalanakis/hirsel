package dev.hirsel.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ThreadNotificationDataTest {
    @Test fun parserCarriesHistoryIntoIntentAndCurrentRouting() {
        val notification = parseThreadNotificationData(
            mapOf("title" to "Choose", "thread_id" to "42", "history_id" to "history-a"),
        )!!

        assertEquals(
            mapOf("thread_id" to "42", "history_id" to "history-a"),
            notification.intentExtras(),
        )
        assertEquals(42uL, notificationDestination(
            notification.intentExtras()["history_id"],
            notification.intentExtras()["thread_id"]?.toULongOrNull(),
            "history-a",
            listOf(42uL),
        ))
        assertNull(notificationDestination(
            notification.intentExtras()["history_id"],
            notification.intentExtras()["thread_id"]?.toULongOrNull(),
            "history-b",
            listOf(42uL),
        ))
    }

    @Test fun parserRejectsMissingOrInvalidDestinationFields() {
        val valid = mapOf("title" to "Choose", "thread_id" to "42", "history_id" to "history-a")
        for (key in valid.keys) {
            assertNull(parseThreadNotificationData(valid - key))
        }
        assertNull(parseThreadNotificationData(valid + ("thread_id" to "not-a-number")))
        assertNull(parseThreadNotificationData(valid + ("history_id" to "  ")))
    }
}
