package dev.hirsel.android.pairing

import dev.hirsel.core.ClientSnapshot
import dev.hirsel.core.ConnectionState
import dev.hirsel.core.FfiConverterTypeClientSnapshot
import dev.hirsel.core.FfiConverterTypeLifecycleEvent
import dev.hirsel.core.FfiConverterTypeThreadRelatedItem
import dev.hirsel.core.LifecycleEvent
import dev.hirsel.core.ThreadRelatedItem
import dev.hirsel.core.ThreadRelatedTarget
import java.nio.ByteBuffer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

class RelatedItemsBindingTest {
    private fun link(title: String?) = ThreadRelatedItem(
        id = 7uL, threadId = 5uL, target = ThreadRelatedTarget.Url("https://example.com/?q=1#part"),
        title = title, createdAt = "2026-09-10T00:00:00+00:00",
    )

    @Test fun savedLinkRecordPreservesIdentityUrlAndNullableTitle() {
        for (title in listOf(null, "Reference café 📎")) {
            val expected = link(title)
            val buffer = ByteBuffer.allocate(FfiConverterTypeThreadRelatedItem.allocationSize(expected).toInt())
            FfiConverterTypeThreadRelatedItem.write(expected, buffer)
            buffer.flip()
            assertEquals(expected, FfiConverterTypeThreadRelatedItem.read(buffer))
            assertFalse(buffer.hasRemaining())
        }
    }

    @Test fun snapshotCarriesSavedLinksAndExplicitEmptyResetList() {
        val loaded = ClientSnapshot(
            connection = ConnectionState.ONLINE, messages = emptyList(), threads = emptyList(),
            turns = emptyList(), activities = emptyList(), briefs = emptyList(),
            relatedItems = listOf(link("Reference"), link(null).copy(
                id = 8uL, target = ThreadRelatedTarget.Thread(historyId = "old", threadId = 0uL),
            )), streams = emptyList(),
            openedThreads = listOf(5uL), historyHasMore = listOf(5uL), createdThreads = emptyList(),
            historyId = "old", recoveredDrafts = emptyList(), hostVersion = "test",
        )
        for (expected in listOf(loaded, loaded.copy(historyId = "new", relatedItems = emptyList()))) {
            val buffer = ByteBuffer.allocate(FfiConverterTypeClientSnapshot.allocationSize(expected).toInt())
            FfiConverterTypeClientSnapshot.write(expected, buffer)
            buffer.flip()
            assertEquals(expected, FfiConverterTypeClientSnapshot.read(buffer))
            assertFalse(buffer.hasRemaining())
        }
    }

    @Test fun commandCallbacksPreserveRequestAndHistoryIdentity() {
        val events = listOf(
            LifecycleEvent.ThreadActionApplied(clientId = "action", historyId = "A", threadId = 5uL),
            LifecycleEvent.ThreadRelatedChanged(historyId = "A", threadId = 5uL, clientId = "saved"),
            LifecycleEvent.ThreadRelatedChanged(historyId = "A", threadId = 5uL, clientId = null),
            LifecycleEvent.ProtocolError(detail = "History changed", clientId = "delayed"),
        )
        for (expected in events) {
            val buffer = ByteBuffer.allocate(FfiConverterTypeLifecycleEvent.allocationSize(expected).toInt())
            FfiConverterTypeLifecycleEvent.write(expected, buffer)
            buffer.flip()
            assertEquals(expected, FfiConverterTypeLifecycleEvent.read(buffer))
            assertFalse(buffer.hasRemaining())
        }
    }
}

class ActionFailureOwnershipTest {
    @Test fun targetedFailuresOnlyRenderInTheirOwningHistoryAndThread() {
        val targeted = ActionFailure("Archive rejected", "A", 5uL, "action")
        assertEquals(true, targeted.visibleIn("A", 5uL))
        assertEquals(false, targeted.visibleIn("A", 6uL))
        assertEquals(false, targeted.visibleIn("B", 5uL))
        assertEquals(true, ActionFailure("Host unavailable").visibleIn("B", 6uL))
    }
}

class RelatedNavigationTest {
    @Test fun typedNavigationRequiresOnlineMatchingHistoryAndExistingTarget() {
        val target = ThreadRelatedTarget.Thread(historyId = "A", threadId = 0uL)
        assertEquals(0uL, relatedThreadDestination(target, ConnectionState.ONLINE, "A", listOf(0uL, 5uL)))
        assertEquals(null, relatedThreadDestination(target, ConnectionState.ONLINE, "B", listOf(0uL, 5uL)))
        assertEquals(null, relatedThreadDestination(target, ConnectionState.ONLINE, null, listOf(0uL)))
        assertEquals(null, relatedThreadDestination(target, ConnectionState.ONLINE, "A", listOf(5uL)))
        for (state in listOf(ConnectionState.CONNECTING, ConnectionState.OFFLINE)) {
            assertEquals(null, relatedThreadDestination(target, state, "A", listOf(0uL)))
        }
        assertEquals(null, relatedThreadDestination(
            ThreadRelatedTarget.Url("https://example.com/t/0?history=A"),
            ConnectionState.ONLINE, "A", listOf(0uL),
        ))
    }
}
