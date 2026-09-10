package dev.hirsel.android

import android.content.Context
import dev.hirsel.android.settings.SETTINGS_PREFS
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

internal data class PushRegistration(
    val token: String?,
    val enabled: Boolean,
)

/** The process-observable state and durable write boundary for Android push registration. */
internal class PushRegistrationOwner(
    initial: PushRegistration,
    private val persist: (PushRegistration) -> Unit,
) {
    private val mutableState = MutableStateFlow(initial)
    val state: StateFlow<PushRegistration> = mutableState.asStateFlow()

    @Synchronized
    fun recordToken(token: String): Boolean {
        if (token.isBlank()) return false
        update(mutableState.value.copy(token = token))
        return true
    }

    @Synchronized
    fun setEnabled(enabled: Boolean) {
        update(mutableState.value.copy(enabled = enabled))
    }

    private fun update(next: PushRegistration) {
        if (next == mutableState.value) return
        persist(next)
        mutableState.value = next
    }
}

internal sealed interface PushRegistrationAction {
    data object Idle : PushRegistrationAction
    data object Fetch : PushRegistrationAction
    data class Register(val token: String) : PushRegistrationAction
}

internal fun pushRegistrationAction(
    registration: PushRegistration,
    online: Boolean,
): PushRegistrationAction = when {
    !online || !registration.enabled -> PushRegistrationAction.Idle
    registration.token == null -> PushRegistrationAction.Fetch
    else -> PushRegistrationAction.Register(registration.token)
}

internal suspend fun executePushRegistrationAction(
    action: PushRegistrationAction,
    fetchToken: suspend () -> String,
    recordToken: (String) -> Boolean,
    registerToken: suspend (String) -> Unit,
) {
    when (action) {
        PushRegistrationAction.Idle -> Unit
        PushRegistrationAction.Fetch -> check(recordToken(fetchToken())) {
            "Firebase returned an empty FCM token"
        }
        is PushRegistrationAction.Register -> registerToken(action.token)
    }
}

/**
 * Keeps the latest FCM token and notification preference in the existing
 * app-private settings file. The singleton lets Firebase callbacks publish to
 * an already-running Activity without opening another connection.
 */
internal class PushRegistrationStore private constructor(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(SETTINGS_PREFS, Context.MODE_PRIVATE)
    private val owner = PushRegistrationOwner(
        initial = PushRegistration(
            token = prefs.getString(KEY_TOKEN, null)?.takeIf { it.isNotBlank() },
            enabled = prefs.getBoolean(KEY_ENABLED, true),
        ),
        persist = { registration ->
            prefs.edit()
                .putString(KEY_TOKEN, registration.token)
                .putBoolean(KEY_ENABLED, registration.enabled)
                .apply()
        },
    )

    val state: StateFlow<PushRegistration> = owner.state

    fun recordToken(token: String): Boolean = owner.recordToken(token)

    fun setEnabled(enabled: Boolean) = owner.setEnabled(enabled)

    companion object {
        private const val KEY_TOKEN = "fcm_token"
        private const val KEY_ENABLED = "push_enabled"

        @Volatile
        private var instance: PushRegistrationStore? = null

        fun get(context: Context): PushRegistrationStore = instance ?: synchronized(this) {
            instance ?: PushRegistrationStore(context).also { instance = it }
        }
    }
}
