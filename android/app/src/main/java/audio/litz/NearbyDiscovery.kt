package audio.litz

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.compose.runtime.Immutable
import java.net.Inet6Address
import java.net.InetAddress
import java.util.concurrent.Executor

/** Must match `litz_core::protocol::VERSION`; the computer advertises it over mDNS. */
internal const val PROTOCOL_VERSION = "1"

@Immutable
internal data class NearbyComputer(
    val name: String,
    val fingerprint: String,
    val endpoint: String,
    val endpoints: List<String> = listOf(endpoint),
)

/**
 * DNS-SD supplies addresses, never trust. Resolve serially on older Android versions, where
 * concurrent resolveService calls fail with ALREADY_ACTIVE.
 */
internal class NearbyDiscovery(
    context: Context,
    private val failed: (UiText) -> Unit = {},
    private val changed: (List<NearbyComputer>) -> Unit,
) {
    private val nsd = context.getSystemService(NsdManager::class.java)
    private val main = Handler(Looper.getMainLooper())
    private val entries = linkedMapOf<String, NearbyComputer>()
    private val found = linkedMapOf<String, NsdServiceInfo>()
    private val queue = ArrayDeque<NsdServiceInfo>()
    private var listener: NsdManager.DiscoveryListener? = null
    private var wanted = false
    private var epoch = 0
    private var resolving = false
    private var resolution = 0L
    private var timeout: Runnable? = null
    private val updates = linkedMapOf<String, NsdManager.ServiceInfoCallback>()

    fun start() {
        if (Looper.myLooper() != Looper.getMainLooper()) {
            main.post { start() }
            return
        }
        wanted = true
        if (listener != null) return
        val next =
            object : NsdManager.DiscoveryListener {
                override fun onDiscoveryStarted(type: String) = Unit

                override fun onStartDiscoveryFailed(type: String, code: Int) {
                    main.post {
                        if (listener === this) {
                            stop()
                            failed(UiText(R.string.discovery_failed, listOf(code)))
                        }
                    }
                }

                override fun onStopDiscoveryFailed(type: String, code: Int) {
                    main.post {
                        if (listener === this) {
                            listener = null
                            clear()
                        }
                    }
                }

                override fun onDiscoveryStopped(type: String) {
                    main.post {
                        if (listener === this) {
                            listener = null
                            if (wanted) runCatching { start() }
                        }
                    }
                }

                override fun onServiceFound(info: NsdServiceInfo) {
                    main.post {
                        if (
                            !wanted ||
                                listener !== this ||
                                !info.serviceType.startsWith("_litz._udp") ||
                                found.size >= 64
                        )
                            return@post
                        found[info.serviceName] = info
                        if (Build.VERSION.SDK_INT >= 34) {
                            watch(info)
                            return@post
                        }
                        if (queue.none { it.serviceName == info.serviceName }) queue.addLast(info)
                        resolveNext()
                    }
                }

                override fun onServiceLost(info: NsdServiceInfo) {
                    main.post {
                        if (listener !== this) return@post
                        found.remove(info.serviceName)
                        if (Build.VERSION.SDK_INT >= 34)
                            updates.remove(info.serviceName)?.let {
                                runCatching { nsd.unregisterServiceInfoCallback(it) }
                            }
                        queue.removeAll { it.serviceName == info.serviceName }
                        entries.remove(info.serviceName)
                        publish()
                    }
                }
            }
        listener = next
        try {
            nsd.discoverServices("_litz._udp.", NsdManager.PROTOCOL_DNS_SD, next)
        } catch (error: Exception) {
            stop()
            failed(UiText(R.string.discovery_unavailable, listOf(error.reason())))
        }
    }

    private fun publish() =
        changed(
            entries.values
                .groupBy { it.fingerprint }
                .values
                .map { matches ->
                    val first = matches.first()
                    val endpoints = matches.flatMap { it.endpoints }.distinct()
                    first.copy(endpoint = endpoints.first(), endpoints = endpoints)
                }
        )

    private fun clear() {
        epoch++
        resolution++
        resolving = false
        timeout?.let(main::removeCallbacks)
        timeout = null
        if (Build.VERSION.SDK_INT >= 34) {
            val callbacks = updates.values.toList()
            updates.clear()
            callbacks.forEach { runCatching { nsd.unregisterServiceInfoCallback(it) } }
        }
        found.clear()
        entries.clear()
        queue.clear()
        publish()
    }

    @android.annotation.TargetApi(34)
    private fun watch(info: NsdServiceInfo) {
        if (updates.containsKey(info.serviceName)) return
        val callback =
            object : NsdManager.ServiceInfoCallback {
                override fun onServiceInfoCallbackRegistrationFailed(errorCode: Int) {
                    if (updates[info.serviceName] !== this) return
                    updates.remove(info.serviceName)
                    failed(UiText(R.string.address_unavailable_code, listOf(errorCode)))
                }

                override fun onServiceUpdated(updated: NsdServiceInfo) {
                    if (updates[info.serviceName] !== this) return
                    parse(updated, updated.hostAddresses)?.let { entries[info.serviceName] = it }
                        ?: entries.remove(info.serviceName)
                    publish()
                }

                override fun onServiceLost() {
                    if (updates[info.serviceName] === this) {
                        entries.remove(info.serviceName)
                        publish()
                    }
                }

                override fun onServiceInfoCallbackUnregistered() = Unit
            }
        updates[info.serviceName] = callback
        try {
            nsd.registerServiceInfoCallback(info, Executor { main.post(it) }, callback)
        } catch (error: Exception) {
            updates.remove(info.serviceName)
            failed(UiText(R.string.address_unavailable, listOf(error.reason())))
        }
    }

    @Suppress("DEPRECATION")
    private fun resolveNext() {
        if (!wanted || resolving || queue.isEmpty()) return
        val info = queue.removeFirst()
        val generation = epoch
        val token = ++resolution
        resolving = true
        fun finish(service: NsdServiceInfo?) {
            if (generation != epoch || token != resolution) return
            resolution++
            timeout?.let(main::removeCallbacks)
            timeout = null
            resolving = false
            if (wanted && found[info.serviceName] === info && service != null) {
                parse(service, listOfNotNull(service.host))?.let { entries[info.serviceName] = it }
                publish()
            }
            resolveNext()
        }
        timeout =
            Runnable {
                    if (generation == epoch && token == resolution) {
                        finish(null)
                        failed(UiText(R.string.address_timed_out))
                    }
                }
                .also { main.postDelayed(it, 5000) }
        val callback =
            object : NsdManager.ResolveListener {
                override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                    main.post {
                        if (generation == epoch && token == resolution) {
                            finish(null)
                            failed(UiText(R.string.address_failed, listOf(errorCode)))
                        }
                    }
                }

                override fun onServiceResolved(serviceInfo: NsdServiceInfo) {
                    main.post { finish(serviceInfo) }
                }
            }
        try {
            nsd.resolveService(info, callback)
        } catch (error: Exception) {
            finish(null)
            failed(UiText(R.string.address_unavailable, listOf(error.reason())))
        }
    }

    @Suppress("DEPRECATION")
    private fun parse(info: NsdServiceInfo, addresses: List<InetAddress>): NearbyComputer? {
        val version = info.attributes["v"]?.toString(Charsets.UTF_8)
        val fingerprint = info.attributes["id"]?.toString(Charsets.UTF_8)?.lowercase()
        if (
            version != PROTOCOL_VERSION ||
                fingerprint?.matches(Regex("[0-9a-f]{64}")) != true ||
                info.port !in 1024..65535
        )
            return null
        val endpoints =
            addresses
                .filterNot { it.isAnyLocalAddress || it.isMulticastAddress }
                .sortedBy { it is Inet6Address }
                .mapNotNull { host ->
                    if (host is Inet6Address) {
                        val base = host.hostAddress?.substringBefore('%') ?: return@mapNotNull null
                        val scope = host.scopeId.takeIf { it != 0 }?.let { "%$it" } ?: ""
                        "[$base$scope]:${info.port}"
                    } else "${host.hostAddress}:${info.port}"
                }
                .distinct()
                .take(16)
        return endpoints.firstOrNull()?.let {
            NearbyComputer(info.serviceName.take(64), fingerprint, it, endpoints)
        }
    }

    fun refresh() {
        if (Looper.myLooper() != Looper.getMainLooper()) {
            main.post { refresh() }
            return
        }
        stop()
        start()
    }

    fun stop() {
        if (Looper.myLooper() != Looper.getMainLooper()) {
            main.post { stop() }
            return
        }
        wanted = false
        clear()
        val previous = listener
        listener = null
        previous?.let { runCatching { nsd.stopServiceDiscovery(it) } }
    }
}
