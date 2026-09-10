package dev.hirsel.android.settings

import android.content.Context
import dev.hirsel.android.ui.ThemeMode

/** Which pings raise a push notification. */
enum class NotifyScope { REQUIRES_RESPONSE, ALL }

/**
 * User preferences (theme, notification scope, debug), persisted in app-private
 * SharedPreferences. Reads are synchronous so the chosen theme is known before
 * the first Compose paint — no light/dark flash on cold start. Push registration
 * state has its own observable owner; authentication credentials live separately
 * in the encrypted [TokenStore].
 */
class SettingsStore(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(SETTINGS_PREFS, Context.MODE_PRIVATE)

    var themeMode: ThemeMode
        get() = runCatching { ThemeMode.valueOf(prefs.getString(KEY_THEME, null) ?: "") }
            .getOrDefault(ThemeMode.SYSTEM)
        set(value) { prefs.edit().putString(KEY_THEME, value.name).apply() }

    var notifyScope: NotifyScope
        get() = runCatching { NotifyScope.valueOf(prefs.getString(KEY_SCOPE, null) ?: "") }
            .getOrDefault(NotifyScope.ALL)
        set(value) { prefs.edit().putString(KEY_SCOPE, value.name).apply() }

    var debugMode: Boolean
        get() = prefs.getBoolean(KEY_DEBUG, false)
        set(value) { prefs.edit().putBoolean(KEY_DEBUG, value).apply() }

    private companion object {
        const val KEY_THEME = "theme_mode"
        const val KEY_SCOPE = "notify_scope"
        const val KEY_DEBUG = "debug_mode"
    }
}

internal const val SETTINGS_PREFS = "hirsel_settings"
