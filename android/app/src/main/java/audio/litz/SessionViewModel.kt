package audio.litz

import android.app.Application
import android.content.*
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.Build
import android.os.IBinder
import android.os.SystemClock
import androidx.compose.runtime.Immutable
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import audio.litz.native.Failure
import audio.litz.native.Pairing
import audio.litz.native.PairingState
import audio.litz.native.Route
import audio.litz.native.SavedComputer
import audio.litz.native.Settings
import audio.litz.native.VoiceSettings
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

internal fun retryDelay(seen: Boolean, failures: Int): Long =
    if (seen) 2_000L shl failures.coerceIn(0, 3) else 60_000L

private const val IDLE_CLOSE_MS = 30_000L

sealed interface SessionLifecycle {
    data object Idle : SessionLifecycle

    data class Connecting(val endpoint: String) : SessionLifecycle

    data class Connected(val desktop: String) : SessionLifecycle

    data class Streaming(val desktop: String, val running: List<Boolean>) : SessionLifecycle
}

@Immutable
internal data class UiState(
    val settings: Settings = Settings(),
    val session: SessionStatus = SessionStatus(),
    val paired: Boolean = false,
    val desktop: String = "",
    val message: UiText? = null,
    val ready: Boolean = false,
    val nearby: List<NearbyComputer> = emptyList(),
    val incompatible: List<NearbyComputer> = emptyList(),
    val saved: List<SavedComputer> = emptyList(),
    val selected: String? = null,
    val pairing: String? = null,
    val pairingCode: String? = null,
    val pairingConfirm: Boolean = false,
    val discovering: Boolean = false,
    val quiet: Boolean = false,
) {
    val lifecycle: SessionLifecycle
        get() {
            val snapshot = Snapshot(session)
            val runningStreams = (0..1).filter { snapshot.running(it) }
            return when {
                runningStreams.isNotEmpty() ->
                    SessionLifecycle.Streaming(desktop, runningStreams.map { true })
                ready && snapshot.connected -> SessionLifecycle.Connected(desktop)
                session.pending -> SessionLifecycle.Connecting(desktop)
                else -> SessionLifecycle.Idle
            }
        }
}

class SessionViewModel(application: Application) : AndroidViewModel(application) {
    private val context
        get() = getApplication<Application>()

    private val mutable = MutableStateFlow(UiState())
    internal val state = mutable.asStateFlow()
    // This Application-bound service is intentionally retained until onCleared;
    // no Activity context is retained, and the binding is explicitly released.
    @android.annotation.SuppressLint("StaticFieldLeak") private var service: SessionService? = null
    private var collection: Job? = null
    private var draft: PreferenceDraft? = null
    private val discovery =
        NearbyDiscovery(
            context,
            failed = { reason -> mutable.update { it.copy(message = reason, discovering = false) } },
        ) { nearby ->
            val update = ++discoveryUpdate
            viewModelScope.launch {
                val selectedUsb = usbEndpoint?.takeIf { it.first == mutable.value.selected }?.second
                val candidates =
                    nearby
                        .filter { it.compatible }
                        .map { computer ->
                            if (
                                computer.fingerprint == mutable.value.selected &&
                                    selectedUsb != null
                            )
                                computer.copy(endpoints = computer.endpoints + selectedUsb)
                            else computer
                        }
                val ordered =
                    withContext(Dispatchers.IO) {
                        UsbRoutes.prefer(candidates, UsbRoutes.subnets())
                    }
                if (update != discoveryUpdate) return@launch
                mutable.update {
                    it.copy(nearby = ordered, incompatible = nearby.filterNot { it.compatible })
                }
                ordered
                    .firstOrNull { it.fingerprint == mutable.value.selected }
                    ?.endpoints
                    ?.firstOrNull { endpoint ->
                        endpoint.firstOrNull()?.isDigit() == true &&
                            usbSubnets.none { it.contains(endpoint) }
                    }
                    ?.let { service?.rememberWifiAddress(it) }
                val store = IdentityStore(context)
                var selectedMoved = false
                withContext(Dispatchers.IO) {
                    ordered.forEach {
                        if (
                            !(it.fingerprint == mutable.value.selected &&
                                (mutable.value.settings.mic || mutable.value.settings.playback) &&
                                usbSubnets.isNotEmpty()) &&
                                store.updateAddress(it.fingerprint, it.endpoint) &&
                                it.fingerprint == mutable.value.selected
                        )
                            selectedMoved = true
                    }
                }
                val saved = withContext(Dispatchers.IO) { store.saved() }
                mutable.update { it.copy(saved = saved) }
                if (selectedMoved)
                    saved
                        .firstOrNull { it.fingerprint == mutable.value.selected }
                        ?.let { service?.address(it.endpoint) }
                reconnectIfIdle()
            }
        }
    private var forgetting = false
    private var idleClose: Job? = null

    private var lastAttempt = SystemClock.elapsedRealtime()
    private var visible = false
    private var addressAttempt = 0
    private var usbSubnets: List<UsbSubnet> = emptyList()
    private var usbEndpoint: Pair<String, String>? = null

    internal fun visible(value: Boolean) {
        visible = value
        idleClose?.cancel()
        if (value) reconnectIfIdle()
        else
            idleClose =
                viewModelScope.launch {
                    delay(IDLE_CLOSE_MS)
                    val ui = mutable.value
                    if (!ui.settings.mic && !ui.settings.playback && ui.pairing == null)
                        service?.disconnectAndWait()
                }
    }

    private fun seen(ui: UiState) =
        ui.nearby.any { it.fingerprint == ui.selected } || usbEndpoint?.first == ui.selected

    private fun reconnectIfIdle(): Boolean {
        val ui = mutable.value
        if (
            visible &&
                !forgetting &&
                Snapshot(ui.session).reconnectAllowed &&
                ui.selected != null &&
                ui.pairing == null &&
                !ui.session.pending &&
                !Snapshot(ui.session).connected &&
                !ui.settings.mic &&
                !ui.settings.playback
        ) {
            ui.nearby
                .firstOrNull { it.fingerprint == ui.selected }
                ?.endpoints
                ?.takeIf { it.isNotEmpty() }
                ?.let { service?.address(it[Math.floorMod(addressAttempt++, it.size)]) }
            val service = service ?: return false
            mutable.update { it.copy(quiet = true) }
            service.connect()
            return true
        }
        return false
    }

    private val connectivity = context.getSystemService(ConnectivityManager::class.java)
    private val networkCallback =
        object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                viewModelScope.launch { discovery.refresh() }
            }

            override fun onLost(network: Network) {
                viewModelScope.launch { discovery.refresh() }
            }
        }
    private var networkCallbackRegistered = false
    private val pairLifetime = Any()
    private var pairing: Pairing? = null
    private var pairJob: Job? = null
    private var discoveryUpdate = 0
    private val connection =
        object : ServiceConnection {
            override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
                service = (binder as SessionService.LocalBinder).service
                reconnectIfIdle()
                collection =
                    viewModelScope.launch {
                        service!!.status.collect { status ->
                            val enrolled = status.status?.enrolled == true
                            mutable.update {
                                it.copy(
                                    session = status,
                                    paired = it.paired || enrolled,
                                    settings =
                                        mergePreferences(
                                            if (
                                                status.status != null ||
                                                    status.pending ||
                                                    status.error != null
                                            )
                                                status.settings
                                            else it.settings,
                                            draft,
                                        ),
                                )
                            }
                        }
                    }
            }

            override fun onServiceDisconnected(name: ComponentName?) {
                service = null
                collection?.cancel()
                mutable.update {
                    it.copy(
                        settings = it.settings.copy(mic = false, playback = false),
                        session = SessionStatus(),
                        message = UiText(R.string.message_service_stopped),
                    )
                }
            }
        }

    init {
        viewModelScope.launch {
            var failures = 0
            while (isActive) {
                delay(2000)
                val ui = mutable.value
                if (Snapshot(ui.session).connected) {
                    failures = 0
                    continue
                }
                val now = SystemClock.elapsedRealtime()
                if (now - lastAttempt >= retryDelay(seen(ui), failures) && reconnectIfIdle()) {
                    lastAttempt = now
                    failures++
                }
            }
        }
        viewModelScope.launch {
            while (isActive) {
                delay(3000)
                if (!mutable.value.discovering) continue
                val interfaces = withContext(Dispatchers.IO) { UsbRoutes.interfaces() }
                val current = interfaces.map { it.subnet }
                val selected =
                    mutable.value.saved.firstOrNull { it.fingerprint == mutable.value.selected }
                val found =
                    if (selected != null && interfaces.isNotEmpty())
                        withContext(Dispatchers.IO) {
                            UsbMdns.resolve(
                                selected.fingerprint,
                                selected.endpoint.substringAfterLast(':').toIntOrNull() ?: 47165,
                                interfaces,
                            )
                        }
                    else null
                if (
                    selected != null &&
                        found != null &&
                        usbEndpoint != (selected.fingerprint to found)
                ) {
                    usbEndpoint = selected.fingerprint to found
                    if (!mutable.value.settings.mic && !mutable.value.settings.playback) {
                        val store = IdentityStore(context)
                        withContext(Dispatchers.IO) {
                            store.updateAddress(selected.fingerprint, found)
                        }
                        mutable.update { state ->
                            state.copy(
                                saved =
                                    state.saved.map {
                                        if (it.fingerprint == selected.fingerprint)
                                            it.copy(endpoint = found)
                                        else it
                                    }
                            )
                        }
                        service?.address(found)
                    }
                } else if (interfaces.isEmpty()) {
                    usbEndpoint = null
                }
                if (current != usbSubnets) {
                    usbSubnets = current
                    discovery.refresh()
                }
            }
        }
        context.bindService(
            Intent(context, SessionService::class.java),
            connection,
            Context.BIND_AUTO_CREATE,
        )
        viewModelScope.launch {
            runCatching {
                    val preferences = Preferences(context).read()
                    val store = IdentityStore(context)
                    val identity = withContext(Dispatchers.IO) { store.phone() }
                    val saved = withContext(Dispatchers.IO) { store.saved() }
                    val selected = withContext(Dispatchers.IO) { store.selectedFingerprint() }
                    mutable.update {
                        it.copy(
                            settings =
                                if (it.session.status == null && !it.session.pending) preferences
                                else it.settings,
                            ready = true,
                            paired = selected != null,
                            saved = saved,
                            selected = selected,
                            desktop = identity.endpoint,
                        )
                    }
                    reconnectIfIdle()
                }
                .onFailure { message(it.text()) }
        }
    }

    internal fun message(text: UiText) {
        mutable.update { it.copy(message = text) }
    }

    internal fun startDiscovery() {
        mutable.update { it.copy(discovering = true) }
        runCatching {
                discovery.start()
                if (!networkCallbackRegistered) {
                    connectivity.registerNetworkCallback(
                        NetworkRequest.Builder()
                            .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                            .build(),
                        networkCallback,
                    )
                    networkCallbackRegistered = true
                }
            }
            .onFailure { error ->
                mutable.update {
                    it.copy(
                        discovering = false,
                        message = UiText(R.string.discovery_unavailable, listOf(error.reason())),
                    )
                }
            }
    }

    internal fun refreshDiscovery() {
        mutable.update { it.copy(discovering = true) }
        discovery.refresh()
    }

    internal fun pair(computer: NearbyComputer) {
        if (pairJob != null) return
        mutable.update { it.copy(pairing = computer.name, message = null, quiet = false) }
        pairJob =
            viewModelScope.launch {
                try {
                    service?.disconnectAndWait()
                    val store = IdentityStore(context)
                    val identity = withContext(Dispatchers.IO) { store.phone() }
                    val name =
                        Build.MODEL.take(64).ifBlank {
                            context.getString(R.string.phone_default_name)
                        }
                    val pairing =
                        withContext(NonCancellable + Dispatchers.IO) {
                            Pairing(identity, computer.endpoint, computer.fingerprint, name).also {
                                synchronized(pairLifetime) { this@SessionViewModel.pairing = it }
                            }
                        }
                    while (true) {
                        when (val state = withContext(Dispatchers.IO) { pairing.state() }) {
                            is PairingState.Connecting -> delay(200)
                            is PairingState.Reviewing -> {
                                mutable.update { it.copy(pairingCode = state.code) }
                                delay(200)
                            }
                            is PairingState.Confirming -> {
                                mutable.update {
                                    it.copy(pairingCode = state.code, pairingConfirm = true)
                                }
                                delay(200)
                            }
                            is PairingState.Accepted -> {
                                withContext(Dispatchers.IO) {
                                    store.remember(computer.name, state.certificate, state.endpoint)
                                }
                                val saved = withContext(Dispatchers.IO) { store.saved() }
                                mutable.update {
                                    it.copy(
                                        saved = saved,
                                        selected = state.fingerprint,
                                        paired = true,
                                        desktop = computer.endpoint,
                                        pairing = null,
                                        message =
                                            UiText(R.string.paired_with, listOf(computer.name)),
                                    )
                                }
                                service?.connect()
                                break
                            }
                            is PairingState.Cancelled -> {
                                message(UiText(R.string.pairing_cancelled))
                                break
                            }
                            is PairingState.Failed -> {
                                message(failureText(state.failure))
                                break
                            }
                        }
                    }
                } catch (_: CancellationException) {
                    message(UiText(R.string.pairing_cancelled))
                } catch (e: Failure) {
                    message(failureText(e))
                } catch (e: Exception) {
                    message(UiText(R.string.pairing_failed, listOf(e.reason())))
                } finally {
                    withContext(NonCancellable + Dispatchers.IO) {
                        synchronized(pairLifetime) {
                            pairing?.close()
                            pairing = null
                        }
                    }
                    mutable.update {
                        it.copy(pairing = null, pairingCode = null, pairingConfirm = false)
                    }
                    pairJob = null
                }
            }
    }

    internal fun confirmPair() {
        synchronized(pairLifetime) { pairing?.confirm() }
    }

    internal fun cancelPair() {
        val cancelled = synchronized(pairLifetime) { pairing?.cancel() != null }
        if (!cancelled) pairJob?.cancel()
    }

    internal fun forget() {
        val pin = mutable.value.selected ?: return
        forgetting = true
        viewModelScope.launch {
            try {
                service?.forgetAndWait()
                val saved =
                    withContext(Dispatchers.IO) {
                        val store = IdentityStore(context)
                        store.forget(pin)
                        store.saved()
                    }
                mutable.update {
                    it.copy(
                        saved = saved,
                        selected = null,
                        paired = false,
                        settings = it.settings.copy(mic = false, playback = false),
                        message = UiText(R.string.computer_forgotten),
                    )
                }
            } catch (e: Exception) {
                message(UiText(R.string.computer_forget_failed, listOf(e.reason())))
            } finally {
                forgetting = false
            }
        }
    }

    internal fun select(computer: SavedComputer) {
        mutable.update { it.copy(quiet = false) }
        viewModelScope.launch {
            try {
                service?.disconnectAndWait()
                withContext(Dispatchers.IO) { IdentityStore(context).select(computer.fingerprint) }
                mutable.update {
                    it.copy(
                        selected = computer.fingerprint,
                        paired = true,
                        desktop = computer.endpoint,
                        message = null,
                    )
                }
                service?.connect()
            } catch (e: Exception) {
                message(UiText(R.string.computer_select_failed, listOf(e.reason())))
            }
        }
    }

    internal fun edit(settings: Settings) {
        val before = mutable.value.settings
        draft = (draft ?: PreferenceDraft()).withChanges(before, settings)
        mutable.update { it.copy(settings = settings, message = null) }
        viewModelScope.launch { Preferences(context).writeChanged(before, settings) }
    }

    internal fun connect() {
        lastAttempt = SystemClock.elapsedRealtime()
        mutable.update { it.copy(quiet = false) }
        service?.connect() ?: message(UiText(R.string.message_service_starting))
    }

    internal fun route(revision: ULong, route: Route) {
        service?.route(revision, route)
    }

    internal fun voice(revision: ULong, voice: VoiceSettings) {
        service?.voice(revision, voice)
    }

    internal fun refreshRoutes() {
        service?.refreshRoutes()
    }

    internal fun startAuthorized(settings: Settings) {
        if (!mutable.value.paired) {
            message(UiText(R.string.message_pair_first))
            return
        }
        mutable.update { it.copy(quiet = false) }
        edit(settings)
        draft = null
        try {
            val intent =
                Intent(context, SessionService::class.java)
                    .setAction(SessionService.START)
                    .putExtra("settings", settings.toBundle())
            if (Build.VERSION.SDK_INT >= 26) context.startForegroundService(intent)
            else context.startService(intent)
        } catch (e: Exception) {
            message(e.text())
        }
    }

    internal fun stop() {
        draft = null
        val current = mutable.value.settings.copy(mic = false, playback = false)
        service?.update(current)
            ?: runCatching {
                    context.startService(
                        Intent(context, SessionService::class.java).setAction(SessionService.STOP)
                    )
                }
                .onFailure { message(it.text()) }
    }

    internal fun mute() {
        service?.update(mutable.value.settings.copy(muted = true))
    }

    internal fun applyGains() {
        if (mutable.value.settings.mic || mutable.value.settings.playback) {
            service?.update(mutable.value.settings)
            draft = null
        }
    }

    override fun onCleared() {
        discovery.stop()
        if (networkCallbackRegistered) connectivity.unregisterNetworkCallback(networkCallback)
        cancelPair()
        collection?.cancel()
        context.unbindService(connection)
        service = null
    }
}
