package dev.hirsel.android.pairing

import android.net.Uri

/**
 * A parsed `hirsel://pair?ticket=<iroh-ticket>&code=<pairing-code>` link
 * (PROTOCOL.md v2.3). The URI carries the short-lived, single-use pairing code
 * and never a device token or the static HIRSEL_TOKEN.
 */
data class PairingLink(val ticket: String, val code: String)

sealed interface PairingLinkResult {
    data class Ok(val link: PairingLink) : PairingLinkResult
    data class Invalid(val reason: String) : PairingLinkResult
}

/**
 * Strictly parses a pairing link: requires the `hirsel` scheme, the `pair`
 * authority, and exactly one non-empty value for each of `ticket` and `code`.
 * Anything else is rejected with a legible reason.
 */
fun parsePairingLink(raw: String): PairingLinkResult {
    val trimmed = raw.trim()
    if (trimmed.isEmpty()) {
        return PairingLinkResult.Invalid("Paste a hirsel://pair link to continue.")
    }
    val uri = runCatching { Uri.parse(trimmed) }.getOrNull()
        ?: return PairingLinkResult.Invalid("That doesn't look like a link.")

    if (!uri.scheme.equals("hirsel", ignoreCase = true)) {
        return PairingLinkResult.Invalid("Expected a hirsel:// link.")
    }
    if (!uri.host.equals("pair", ignoreCase = true)) {
        return PairingLinkResult.Invalid("Not a pairing link (expected hirsel://pair).")
    }

    val tickets = uri.getQueryParameters("ticket").filter { it.isNotBlank() }
    val codes = uri.getQueryParameters("code").filter { it.isNotBlank() }
    if (tickets.size != 1) {
        return PairingLinkResult.Invalid("Link is missing its ticket.")
    }
    if (codes.size != 1) {
        return PairingLinkResult.Invalid("Link is missing its pairing code.")
    }
    return PairingLinkResult.Ok(PairingLink(ticket = tickets.first(), code = codes.first()))
}
