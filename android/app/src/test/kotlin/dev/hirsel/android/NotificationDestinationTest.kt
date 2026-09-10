package dev.hirsel.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class NotificationDestinationTest {
    @Test fun rejectsReusedNumericIdFromPriorHistory() {
        assertNull(notificationDestination("old", 1uL, "new", listOf(1uL)))
        assertEquals(1uL, notificationDestination("new", 1uL, "new", listOf(1uL)))
    }
    @Test fun rejectsMissingHistoryAndUnavailableThread() {
        assertNull(notificationDestination(null, 1uL, "new", listOf(1uL)))
        assertNull(notificationDestination("new", 1uL, null, listOf(1uL)))
        assertNull(notificationDestination("new", 1uL, "new", listOf(2uL)))
    }
    @Test fun zeroIsAnOrdinaryExplicitDestination() {
        assertEquals(0uL, notificationDestination("current", 0uL, "current", listOf(0uL)))
        assertNull(notificationDestination("current", null, "current", listOf(0uL)))
    }
}
