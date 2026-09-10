package dev.hirsel.android.chat

import dev.hirsel.core.Thread

internal data class ThreadRow(val thread: Thread, val depth: Int)

/** A filtered forest keeps each surviving child reachable even when its parent is hidden. */
internal fun threadRows(threads: List<Thread>): List<ThreadRow> {
    val ids = threads.map { it.id }.toSet()
    val children = threads.groupBy { it.parentThreadId }
    val order = compareBy<Thread> { if (it.parentThreadId == null && it.pinnedAt != null) 0 else 1 }
        .thenBy { if (it.parentThreadId == null) it.pinnedAt?.let(java.time.Instant::parse) else null }.thenBy { it.id }
    val roots = threads.filter { it.parentThreadId == null || it.parentThreadId !in ids }.sortedWith(order)
    val seen = mutableSetOf<ULong>()
    val result = mutableListOf<ThreadRow>()
    val stack = ArrayDeque<ThreadRow>()
    fun drain() {
        while (stack.isNotEmpty()) {
            val row = stack.removeLast()
            if (!seen.add(row.thread.id)) continue
            result.add(row)
            children[row.thread.id].orEmpty().sortedBy { it.id }.asReversed().forEach {
                stack.addLast(ThreadRow(it, row.depth + 1))
            }
        }
    }
    roots.asReversed().forEach { stack.addLast(ThreadRow(it, 0)) }
    drain()
    // Remain legible if a malformed remote forest ever contains a cycle.
    threads.sortedWith(order).forEach {
        if (it.id !in seen) { stack.addLast(ThreadRow(it, 0)); drain() }
    }
    return result
}
