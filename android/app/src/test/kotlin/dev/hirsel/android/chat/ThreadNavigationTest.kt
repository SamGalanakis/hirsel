package dev.hirsel.android.chat

import dev.hirsel.core.Thread
import dev.hirsel.core.ThreadTint
import dev.hirsel.core.ThreadKind
import org.junit.Assert.assertEquals
import org.junit.Test

class ThreadNavigationTest {
    private fun thread(id: ULong, parent: ULong? = null, pin: String? = null) = Thread(
        kind = ThreadKind.SPACE, parentThreadId = parent, pinnedAt = pin, id = id, icon = null,
        showcasedArtifactId = null, title = "Thread $id",
        description = "", instrumentJson = "null", needsOwner = false,
        settledAt = null, archivedAt = null, snoozedUntil = null, read = true,
        createdAt = "2026-09-10T00:00:00Z", updatedAt = "2026-09-10T00:00:00Z",
        revision = 1uL, runningTurn = null, queuedTurnCount = 0uL,
        lastFinishedTurn = null, lastActivityAt = "2026-09-10T00:00:00Z",
    )

    @Test fun monogramsCoverOneWordTwoWordsAndEmptyTitles() {
        assertEquals("T0", threadIconText(thread(0uL)))
        assertEquals("OB", threadMonogram("Orchard Beds"))
        assertEquals("RT", threadMonogram("  rebuild the android apk "))
        assertEquals("#", threadMonogram("   "))
    }

    @Test fun everyVocabularySymbolHasArtworkAndATintedTile() {
        assertEquals(45, THREAD_SYMBOL_DRAWABLES.size)
        THREAD_SYMBOL_DRAWABLES.values.forEach { org.junit.Assert.assertNotEquals(0, it) }
        ThreadTint.entries.forEach { tint ->
            listOf(true, false).forEach { isLight ->
                val (tile, glyph) = threadTintColors(tint, isLight)
                org.junit.Assert.assertNotEquals(tile, glyph)
            }
        }
    }

    @Test fun spacesHideCompletingInstrumentActionsButKeepContinueActions() {
        assertEquals(false, instrumentActionVisible(true, ThreadKind.SPACE))
        assertEquals(true, instrumentActionVisible(false, ThreadKind.SPACE))
        assertEquals(true, instrumentActionVisible(true, ThreadKind.TASK))
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
