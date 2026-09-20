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

    @Test
    fun toolOutcomeInterruptsTheSameReasoningBlockId() {
        val events = listOf(
            TimelineTextEvent("tool_start", "", null),
            TimelineTextEvent("reasoning", "first", "reasoning-1"),
            TimelineTextEvent("tool_done", "", null),
            TimelineTextEvent("reasoning", "second", "reasoning-1")
        )

        assertEquals("first\n\nsecond", timelineText(events, "reasoning"))
    }
}
