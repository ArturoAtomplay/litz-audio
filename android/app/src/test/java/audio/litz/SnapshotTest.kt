package audio.litz

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class SnapshotTest {
    @Test
    fun connectedEndpointIsReadyForTheUsbCheck() {
        val endpoint = "192.168.42.2:47165"
        val status = Snapshot(SessionStatus(json = """{"connected_endpoint":"$endpoint"}"""))
        assertEquals(endpoint, status.endpoint)
        assertEquals(true, UsbSubnet(0xc0a82a81.toInt(), 24).contains(status.endpoint!!))
        assertNull(Snapshot(SessionStatus(json = """{"connected_endpoint":null}""")).endpoint)
        assertNull(Snapshot(SessionStatus()).endpoint)
    }
}
