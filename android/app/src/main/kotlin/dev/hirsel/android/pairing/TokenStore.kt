package dev.hirsel.android.pairing

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * A paired device's persisted credential: the iroh ticket used to reach the host,
 * the long random device token issued at pairing (NodeId-pinned, revocable), and
 * the human label the token was minted for.
 */
data class DeviceCredential(
    val ticket: String,
    val deviceToken: String,
    val deviceLabel: String,
)

/**
 * Persists the issued device token securely. The values live in an
 * AES-256 EncryptedSharedPreferences file whose master key is held in the Android
 * Keystore — never in plaintext prefs. The iroh secret identity that pins the
 * token lives in the Rust client-core data dir; this store keeps the token that
 * authenticates that identity on reconnect.
 */
class TokenStore(context: Context) {
    private val prefs: SharedPreferences by lazy {
        val masterKey = MasterKey.Builder(context.applicationContext)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            context.applicationContext,
            PREFS_NAME,
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    fun load(): DeviceCredential? {
        val ticket = prefs.getString(KEY_TICKET, null) ?: return null
        val token = prefs.getString(KEY_TOKEN, null) ?: return null
        val label = prefs.getString(KEY_LABEL, null) ?: return null
        if (ticket.isBlank() || token.isBlank()) return null
        return DeviceCredential(ticket = ticket, deviceToken = token, deviceLabel = label)
    }

    fun save(credential: DeviceCredential) {
        prefs.edit()
            .putString(KEY_TICKET, credential.ticket)
            .putString(KEY_TOKEN, credential.deviceToken)
            .putString(KEY_LABEL, credential.deviceLabel)
            .apply()
    }

    /** Local forget. Server-side revoke is a separate host action (/debug/revoke-device). */
    fun clear() {
        prefs.edit().clear().apply()
    }

    private companion object {
        const val PREFS_NAME = "hirsel_device_credential"
        const val KEY_TICKET = "ticket"
        const val KEY_TOKEN = "device_token"
        const val KEY_LABEL = "device_label"
    }
}
