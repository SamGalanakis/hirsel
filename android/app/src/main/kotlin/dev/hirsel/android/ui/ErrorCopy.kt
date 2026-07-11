package dev.hirsel.android.ui

/**
 * Single place that turns raw transport/protocol error strings (from client-core
 * and iroh — e.g. "iroh stream ended", "connection reset by peer") into calm,
 * actionable copy. No raw library text is ever rendered in the UI; every surface
 * that shows a connection or pairing failure routes through here.
 */
object ErrorCopy {

    /** Copy for a live-connection failure/offline reason during chat. */
    fun connection(raw: String?): String {
        val d = raw?.lowercase().orEmpty()
        return when {
            d.isBlank() -> "Couldn't reach your host."
            d.containsAny("stream ended", "reset", "closed", "broken pipe", "eof", "aborted") ->
                "Lost connection to your host — reconnecting…"
            d.containsAny("timeout", "timed out", "deadline") ->
                "Your host is taking a while to answer — retrying…"
            d.containsAny("unreachable", "no route", "dns", "resolve", "relay", "network is") ->
                "Couldn't reach your host. Make sure it's online."
            d.containsAny("refused", "denied", "unauthorized", "forbidden") ->
                "Your host declined the connection. Try re-pairing."
            else -> "Lost connection to your host — reconnecting…"
        }
    }

    /** Copy for a failed pairing handshake. Keeps the actionable code/label cases
     *  and never leaks a raw library string for anything else. */
    fun pairing(raw: String?): String {
        val d = raw?.lowercase().orEmpty()
        return when {
            d.contains("pairing code") ->
                "This pairing code is expired or already used. Generate a fresh one on your host."
            d.contains("device_label") || d.contains("device label") ->
                "This device's name doesn't match the pairing code. Re-pair with the name the host expects."
            d.containsAny("timeout", "timed out", "deadline") ->
                "Your host didn't answer in time. Check it's online and try again."
            d.containsAny("unreachable", "no route", "dns", "resolve", "relay", "network is") ->
                "Couldn't reach your host. Make sure it's online, then scan again."
            d.isBlank() -> "The host didn't answer. Try another link."
            else -> "Couldn't complete pairing. Generate a fresh link on your host and try again."
        }
    }

    private fun String.containsAny(vararg needles: String): Boolean =
        needles.any { this.contains(it) }
}
