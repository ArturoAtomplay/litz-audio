package audio.litz

import audio.litz.native.Engine
import audio.litz.native.Failure
import audio.litz.native.Pairing
import audio.litz.native.SessionState
import audio.litz.native.Settings
import audio.litz.native.createIdentity
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/** Calls the real Rust library, built for this machine, through the generated bindings. */
class NativeBridgeTest {
    @Test
    fun theEngineAnswersWithTypedStatus() {
        val phone = createIdentity()
        assertTrue(phone.certificate.isNotEmpty() && phone.desktop.isEmpty())
        Engine(phone.copy(desktop = phone.certificate, endpoint = "127.0.0.1:9")).use { engine ->
            val status = engine.set(Settings(mic = true, reconnect = false))
            assertFalse(status.microphone.running)
            assertNotNull(status.error)
            val stopped = engine.stop()
            assertFalse(stopped.desired.mic)
            assertEquals(SessionState.IDLE, stopped.state)
        }
    }

    @Test
    fun aRustFailureArrivesAsItsKotlinException() {
        assertThrows(Failure.Internal::class.java) {
            Pairing(createIdentity(), "0.0.0.0:47165", "", "Phone").close()
        }
    }
}
