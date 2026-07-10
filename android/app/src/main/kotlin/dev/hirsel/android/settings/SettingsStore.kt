package dev.hirsel.android.settings

import android.content.Context
import dev.hirsel.android.ui.ThemeMode

/** Which pings raise a push notification. */
enum class NotifyScope { REQUIRES_RESPONSE, ALL }

/**
 * Non-secret user preferences (theme, notifications, debug), persisted in plain
 * SharedPreferences. Reads are synchronous so the chosen theme is known before
 * the first Compose paint — no light/dark flash on cold start. Secrets (device
 * token, iroh identity) live separately in the encrypted [TokenStore].
 */
class SettingsStore(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    var themeMode: ThemeMode
        get() = runCatching { ThemeMode.valueOf(prefs.getString(KEY_THEME, null) ?: "") }
            .getOrDefault(ThemeMode.SYSTEM)
        set(value) { prefs.edit().putString(KEY_THEME, value.name).apply() }

    var pushEnabled: Boolean
        get() = prefs.getBoolean(KEY_PUSH, true)
        set(value) { prefs.edit().putBoolean(KEY_PUSH, value).apply() }

    var notifyScope: NotifyScope
        get() = runCatching { NotifyScope.valueOf(prefs.getString(KEY_SCOPE, null) ?: "") }
            .getOrDefault(NotifyScope.ALL)
        set(value) { prefs.edit().putString(KEY_SCOPE, value.name).apply() }

    var debugMode: Boolean
        get() = prefs.getBoolean(KEY_DEBUG, false)
        set(value) { prefs.edit().putBoolean(KEY_DEBUG, value).apply() }

    private companion object {
        const val PREFS = "hirsel_settings"
        const val KEY_THEME = "theme_mode"
        const val KEY_PUSH = "push_enabled"
        const val KEY_SCOPE = "notify_scope"
        const val KEY_DEBUG = "debug_mode"
    }
}
