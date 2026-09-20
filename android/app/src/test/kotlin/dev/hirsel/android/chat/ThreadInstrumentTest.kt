package dev.hirsel.android.chat

import org.junit.Assert.assertEquals
import org.junit.Test

class ThreadInstrumentTest {
    @Test
    fun reasoningBlocksKeepParagraphBoundariesWhileChunksJoin() {
        val events = listOf(
            TimelineTextEvent("reasoning", "**First ", "first"),
            TimelineTextEvent("reasoning", "thought.**", "first"),
            TimelineTextEvent("reasoning", "**Second thought.**", "second")
        )

        assertEquals(
            "**First thought.**\n\n**Second thought.**",
            timelineText(events, "reasoning")
        )
    }

    @Test
    fun legacyChunksStillJoinAndMixedKindsBreakRuns() {
        val events = listOf(
            TimelineTextEvent("prose", "old ", null),
            TimelineTextEvent("prose", "frame", null),
            TimelineTextEvent("reasoning", "aside", null),
            TimelineTextEvent("prose", "next", null)
        )

        assertEquals("old frame\n\nnext", timelineText(events, "prose"))
    }
}
