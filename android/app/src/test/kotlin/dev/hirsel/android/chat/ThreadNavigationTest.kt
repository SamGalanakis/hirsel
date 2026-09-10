package dev.hirsel.android.chat

import dev.hirsel.core.Thread
import org.junit.Assert.assertEquals
import org.junit.Test

class ThreadNavigationTest {
    private fun thread(id: ULong, parent: ULong? = null, pin: String? = null) = Thread(
        parentThreadId = parent, pinnedAt = pin, id = id, icon = null,
        showcasedArtifactId = null, title = "Thread $id",
        description = "", instrumentJson = "null", needsOwner = false,
        settledAt = null, archivedAt = null, snoozedUntil = null, read = true,
        createdAt = "2026-09-10T00:00:00Z", updatedAt = "2026-09-10T00:00:00Z",
        revision = 1uL, runningTurn = null, queuedTurnCount = 0uL,
        lastFinishedTurn = null, lastActivityAt = "2026-09-10T00:00:00Z",
    )

    @Test fun iconsSupportCustomResetAndUnicodeBounds() {
        assertEquals("T", threadIconText(thread(0uL)))
        assertEquals("👩🏽‍💻", threadIconText(thread(1uL).copy(icon = "👩🏽‍💻")))
        assertEquals("T", threadIconText(thread(1uL).copy(icon = null)))
        listOf(null, "🌱", "👩🏽‍💻", "⭐".repeat(16)).forEach { assertEquals(null, threadIconError(it)) }
        listOf("", " ", "x\n", "x\u0085", "x\u2028", "x\u2029", "x".repeat(17)).forEach {
            org.junit.Assert.assertNotNull(threadIconError(it))
        }
    }

    @Test fun showsOrdinaryZeroAndNestedChildrenWithoutDuplicates() {
        val rows = threadRows(listOf(thread(2uL, 1uL), thread(0uL), thread(1uL, 0uL)))
        assertEquals(listOf(0uL, 1uL, 2uL), rows.map { it.thread.id })
        assertEquals(listOf(0, 1, 2), rows.map { it.depth })
    }

    @Test fun filteredParentDoesNotHideItsChildAndLegacyChildPinsDoNotReorderSiblings() {
        val rows = threadRows(listOf(thread(3uL, 1uL), thread(2uL, 1uL, "2026-09-10T00:00:00Z")))
        assertEquals(listOf(2uL, 3uL), rows.map { it.thread.id })
        assertEquals(listOf(0, 0), rows.map { it.depth })
    }

    @Test fun pinnedRootsStayFirstOnceWithTheirChildren() {
        val rows = threadRows(listOf(thread(0uL), thread(3uL, 1uL, "2026-09-08T00:00:00Z"), thread(2uL, 1uL), thread(1uL, pin = "2026-09-10T00:00:00Z")))
        assertEquals(listOf(1uL, 2uL, 3uL, 0uL), rows.map { it.thread.id })
        assertEquals(listOf(0, 1, 1, 0), rows.map { it.depth })
    }

    @Test fun malformedCycleIsBoundedAndKeepsEveryThreadReachable() {
        val rows = threadRows(listOf(thread(1uL, 2uL), thread(2uL, 1uL)))
        assertEquals(setOf(1uL, 2uL), rows.map { it.thread.id }.toSet())
        assertEquals(2, rows.size)
    }
}
