package audio.litz

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Looper
import androidx.test.core.app.ApplicationProvider
import java.net.InetAddress
import java.time.Duration
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadow.api.Shadow
import org.robolectric.shadows.ShadowNsdManager

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [33])
class NearbyDiscoveryTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val nsd =
        Shadow.extract<ShadowNsdManager>(context.getSystemService(NsdManager::class.java))
    private var computers = emptyList<NearbyComputer>()
    private val errors = mutableListOf<UiText>()
    private val discovery = NearbyDiscovery(context, failed = { errors += it }) { computers = it }

    private fun drain() = shadowOf(Looper.getMainLooper()).idle()

    private fun listener() = requireNotNull(nsd.getDiscoveryListeners("_litz._udp.")).last()

    private fun resolver(info: NsdServiceInfo) =
        requireNotNull(nsd.getResolveListeners(info)).last()

    @Suppress("DEPRECATION")
    private fun info(name: String = "Studio", ip: String = "192.0.2.1") =
        NsdServiceInfo().apply {
            serviceName = name
            serviceType = "_litz._udp."
            port = 47165
            host = InetAddress.getByName(ip)
            setAttribute("v", PROTOCOL_VERSION)
            setAttribute("id", "a".repeat(64))
        }

    @Test
    fun refresh_recovers_missing_callback_and_rejects_late_result() {
        discovery.start()
        val old = info()
        listener().onServiceFound(old)
        drain()
        val stale = resolver(old)
        discovery.refresh()
        val fresh = info(ip = "192.0.2.2")
        listener().onServiceFound(fresh)
        drain()
        stale.onServiceResolved(old)
        drain()
        assertTrue(computers.isEmpty())
        resolver(fresh).onServiceResolved(fresh)
        drain()
        assertEquals("192.0.2.2:47165", computers.single().endpoint)
        discovery.stop()
    }

    @Test
    fun a_computer_on_another_protocol_version_is_shown_as_needing_an_update() {
        discovery.start()
        val later = info().apply { setAttribute("v", "2") }
        listener().onServiceFound(later)
        drain()
        resolver(later).onServiceResolved(later)
        drain()
        assertFalse(computers.single().compatible)
        assertTrue(computers.single().newer)
        discovery.stop()
    }

    @Test
    fun timeout_advances_queue_and_late_failure_cannot_release_next_request() {
        discovery.start()
        val first = info("First")
        val second = info("Second", "192.0.2.2")
        listener().onServiceFound(first)
        listener().onServiceFound(second)
        drain()
        val stale = resolver(first)
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(5))
        assertTrue(errors.any { it.id == R.string.address_timed_out })
        stale.onResolveFailed(first, 3)
        drain()
        resolver(second).onServiceResolved(second)
        drain()
        assertEquals("Second", computers.single().name)
        listener().onServiceLost(second)
        drain()
        assertTrue(computers.isEmpty())
        discovery.stop()
    }

    @Test
    fun lost_service_and_stop_discard_inflight_resolutions() {
        discovery.start()
        val service = info()
        listener().onServiceFound(service)
        drain()
        val callback = resolver(service)
        listener().onServiceLost(service)
        drain()
        callback.onServiceResolved(service)
        drain()
        assertTrue(computers.isEmpty())
        discovery.stop()
        callback.onServiceResolved(service)
        drain()
        assertTrue(computers.isEmpty())
    }
}
