package audio.litz

import audio.litz.native.Engine
import audio.litz.native.Failure
import audio.litz.native.Pairing
import audio.litz.native.SessionState
import audio.litz.native.Settings
import audio.litz.native.defaultPort
import audio.litz.native.isCaptureSource
import audio.litz.native.manualEndpoint
import audio.litz.native.mdnsHost
import audio.litz.native.registryCreate
import audio.litz.native.registryPhone
import kotlin.concurrent.thread
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
        val phone = registryPhone(registryCreate())
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
    fun aStatusChangeReachesAWaitingReaderAndShutdownReleasesIt() {
        val phone = registryPhone(registryCreate())
        Engine(phone.copy(desktop = phone.certificate, endpoint = "127.0.0.1:9")).use { engine ->
            val first = engine.changes(0UL)
            engine.set(Settings(mic = true, reconnect = false))
            val next = engine.changes(first.version)
            assertTrue(next.version > first.version)
            assertNotNull(next.status.error)
            var released: Throwable? = null
            val waiting = thread {
                released = runCatching { engine.changes(ULong.MAX_VALUE) }.exceptionOrNull()
            }
            engine.shutdown()
            waiting.join(5_000)
            assertTrue(released is Failure.Internal)
        }
    }

    @Test
    fun theAppReadsItsProtocolRulesFromTheEngine() {
        assertEquals("1", PROTOCOL_VERSION)
        assertEquals(47165, defaultPort().toInt())
        assertEquals("litz-aaaaaaaaaaaaaaaa.local", mdnsHost("a".repeat(64)))
        assertEquals("192.168.1.10:47165", manualEndpoint("192.168.1.10"))
        assertEquals(null, manualEndpoint("224.0.0.251"))
        assertTrue(isCaptureSource(7) && !isCaptureSource(2))
    }

    @Test
    fun aRustFailureArrivesAsItsKotlinException() {
        assertThrows(Failure.Internal::class.java) {
            Pairing(registryPhone(registryCreate()), "0.0.0.0:47165", "", "Phone").close()
        }
    }
}
