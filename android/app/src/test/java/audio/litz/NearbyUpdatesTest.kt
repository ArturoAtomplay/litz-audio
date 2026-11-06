package audio.litz

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Looper
import androidx.test.core.app.ApplicationProvider
import java.net.InetAddress
import java.util.concurrent.Executor
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements
import org.robolectric.shadow.api.Shadow
import org.robolectric.shadows.ShadowNsdManager

/** Robolectric 4.16's NSD shadow covers only the legacy resolution API. */
@Implements(NsdManager::class)
class UpdatingNsdManager : ShadowNsdManager() {
    val callbacks = mutableListOf<Pair<Executor, NsdManager.ServiceInfoCallback>>()

    @Implementation(minSdk = 34)
    protected fun registerServiceInfoCallback(
        info: NsdServiceInfo,
        executor: Executor,
        callback: NsdManager.ServiceInfoCallback,
    ) {
        callbacks.add(executor to callback)
    }

    @Implementation(minSdk = 34)
    protected fun unregisterServiceInfoCallback(callback: NsdManager.ServiceInfoCallback) {
        callbacks.removeAll { it.second === callback }
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], shadows = [UpdatingNsdManager::class])
class NearbyUpdatesTest {
    @Test
    fun updates_preserve_all_addresses_and_late_callbacks_cannot_restore_lost_presence() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val nsd =
            Shadow.extract<UpdatingNsdManager>(context.getSystemService(NsdManager::class.java))
        var computers = emptyList<NearbyComputer>()
        val discovery = NearbyDiscovery(context) { computers = it }
        val service =
            NsdServiceInfo().apply {
                serviceName = "Studio"
                serviceType = "_litz._udp."
                port = 47165
                setAttribute("v", PROTOCOL_VERSION)
                setAttribute("id", "a".repeat(64))
                hostAddresses =
                    listOf(InetAddress.getByName("2001:db8::1"), InetAddress.getByName("192.0.2.1"))
            }
        discovery.start()
        val listener = requireNotNull(nsd.getDiscoveryListeners("_litz._udp.")).last()
        listener.onServiceFound(service)
        shadowOf(Looper.getMainLooper()).idle()
        val (executor, callback) = nsd.callbacks.single()
        executor.execute { callback.onServiceUpdated(service) }
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals("192.0.2.1:47165", computers.single().endpoint)
        assertEquals(2, computers.single().endpoints.size)
        service.hostAddresses = listOf(InetAddress.getByName("192.0.2.2"))
        executor.execute { callback.onServiceUpdated(service) }
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals("192.0.2.2:47165", computers.single().endpoint)
        listener.onServiceLost(service)
        shadowOf(Looper.getMainLooper()).idle()
        executor.execute { callback.onServiceUpdated(service) }
        shadowOf(Looper.getMainLooper()).idle()
        assertTrue(computers.isEmpty())
        assertTrue(nsd.callbacks.isEmpty())
        discovery.stop()
    }
}
