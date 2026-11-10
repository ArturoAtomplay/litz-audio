package audio.litz

import audio.litz.native.Direction
import audio.litz.native.Failure
import audio.litz.native.SessionState
import audio.litz.native.Settings
import audio.litz.native.Status
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

internal fun status(
    state: SessionState = SessionState.AUTHENTICATED,
    connectedEndpoint: String? = null,
    microphone: Direction = Direction(false, null),
) =
    Status(
        state = state,
        reconnectAllowed = true,
        revoked = false,
        enrolled = true,
        desktop = "192.0.2.1:47165",
        connectedEndpoint = connectedEndpoint,
        desired = Settings(),
        microphone = microphone,
        listening = Direction(false, null),
        owner = null,
        ownershipGeneration = null,
        owned = true,
        desktopRoutes = null,
        error = null,
        microphoneRms = null,
        audio = null,
        callbackFrames = null,
        rttMs = null,
        listeningStats = null,
        transportDrops = null,
    )

class SnapshotTest {
    @Test
    fun connectedEndpointIsReadyForTheUsbCheck() {
        val endpoint = "192.168.42.2:47165"
        val snapshot = Snapshot(SessionStatus(status = status(connectedEndpoint = endpoint)))
        assertEquals(endpoint, snapshot.endpoint)
        assertEquals(true, UsbSubnet(0xc0a82a81.toInt(), 24).contains(snapshot.endpoint!!))
        assertNull(Snapshot(SessionStatus(status = status())).endpoint)
        assertNull(Snapshot(SessionStatus()).endpoint)
    }

    @Test
    fun aDirectionErrorFromTheEngineWinsOverTheLocalOne() {
        val refused = Failure.ComputerRefused(audio.litz.native.Code.ROUTE_CONFLICT)
        val session =
            SessionStatus(
                status = status(microphone = Direction(false, refused)),
                directionErrors = listOf(UiText(R.string.message_directions_stopped), null),
            )
        assertEquals(UiText(R.string.failure_no_virtual_microphone), Snapshot(session).error(0))
        assertNull(Snapshot(session).error(1))
    }
}
