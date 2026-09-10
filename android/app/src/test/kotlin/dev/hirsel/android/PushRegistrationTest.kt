package dev.hirsel.android

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PushRegistrationTest {
    @Test fun tokenRefreshPublishesAndRegistersLatestWithoutReconnect() {
        val persisted = mutableListOf<PushRegistration>()
        val registered = mutableListOf<String>()
        val owner = PushRegistrationOwner(
            PushRegistration(token = "T1", enabled = true),
            persisted::add,
        )

        runBlocking {
            execute(owner, online = true, registered = registered)
        }
        assertTrue(owner.recordToken("T2"))
        assertEquals("T2", owner.state.value.token)
        assertEquals(PushRegistration(token = "T2", enabled = true), persisted.last())
        runBlocking {
            execute(owner, online = true, registered = registered)
        }
        assertEquals(listOf("T1", "T2"), registered)
    }

    @Test fun offlineRefreshDefersLatestTokenUntilOnline() {
        val owner = PushRegistrationOwner(PushRegistration("T1", enabled = true)) {}
        val registered = mutableListOf<String>()

        assertTrue(owner.recordToken("T2"))
        runBlocking {
            execute(owner, online = false, registered = registered)
        }
        assertEquals(emptyList<String>(), registered)
        runBlocking {
            execute(owner, online = true, registered = registered)
        }
        assertEquals(listOf("T2"), registered)
    }

    @Test fun preferenceOffSuppressesAndOnRegistersCurrentToken() {
        val persisted = mutableListOf<PushRegistration>()
        val owner = PushRegistrationOwner(PushRegistration("T1", enabled = true), persisted::add)
        val registered = mutableListOf<String>()

        owner.setEnabled(false)
        assertFalse(owner.state.value.enabled)
        assertEquals(PushRegistration("T1", enabled = false), persisted.last())
        runBlocking {
            execute(owner, online = true, registered = registered)
        }
        assertEquals(emptyList<String>(), registered)
        owner.setEnabled(true)
        assertEquals(PushRegistration("T1", enabled = true), persisted.last())
        runBlocking {
            execute(owner, online = true, registered = registered)
        }
        assertEquals(listOf("T1"), registered)
    }

    @Test fun missingTokenFetchesOnceAndBlankCallbacksDoNotReplaceLatest() {
        val owner = PushRegistrationOwner(PushRegistration(token = null, enabled = true)) {}
        val registered = mutableListOf<String>()

        assertEquals(PushRegistrationAction.Fetch, pushRegistrationAction(owner.state.value, online = true))
        assertFalse(owner.recordToken("  "))
        assertNull(owner.state.value.token)
        runBlocking {
            execute(owner, online = true, registered = registered, fetched = "T1")
        }
        assertEquals("T1", owner.state.value.token)
        assertEquals(emptyList<String>(), registered)
        runBlocking {
            execute(owner, online = true, registered = registered)
        }
        assertEquals(listOf("T1"), registered)
    }

    private suspend fun execute(
        owner: PushRegistrationOwner,
        online: Boolean,
        registered: MutableList<String>,
        fetched: String? = null,
    ) = executePushRegistrationAction(
        action = pushRegistrationAction(owner.state.value, online),
        fetchToken = { requireNotNull(fetched) { "unexpected token fetch" } },
        recordToken = owner::recordToken,
        registerToken = { token -> registered += token },
    )
}
