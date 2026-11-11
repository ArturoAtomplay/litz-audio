package audio.litz

import android.Manifest
import android.app.*
import android.content.*
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.media.*
import android.net.wifi.WifiManager
import android.os.*
import androidx.annotation.StringRes
import androidx.compose.runtime.Immutable
import audio.litz.native.Engine
import audio.litz.native.Route
import audio.litz.native.SessionState
import audio.litz.native.Settings
import audio.litz.native.Status
import audio.litz.native.VoiceSettings
import audio.litz.native.changesAudioPath
import audio.litz.native.defaultPort
import java.util.concurrent.Executors
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

@Immutable
internal data class SessionStatus(
    val settings: Settings = Settings(),
    val status: Status? = null,
    val pending: Boolean = false,
    val error: UiText? = null,
    val directionErrors: List<UiText?> = listOf(null, null),
    val audioPending: Boolean = false,
)

internal data class UsbLink(
    val subnets: List<UsbSubnet> = emptyList(),
    /** The selected computer's fingerprint and its address on the tether. */
    val endpoint: Pair<String, String>? = null,
)

class SessionService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val executor = Executors.newSingleThreadExecutor { Thread(it, "litz-control") }
    private val nativeDispatcher = executor.asCoroutineDispatcher()
    private val mutex = Mutex()
    // Commands run on nativeDispatcher; `lifetime` lets an urgent cancel reach
    // the engine from another thread without racing its close.
    private var engine: Engine? = null
    private val lifetime = Any()
    private var stopGeneration = 0
    private val mutable = MutableStateFlow(SessionStatus())
    internal val status = mutable.asStateFlow()
    private var desired = Settings()
    private lateinit var audio: AudioManager
    private var foreground = false
    private var foregroundTypes = 0
    private var localError: UiText? = null
    private var active = false
    private var controlConnected = false
    private val usbLink = MutableStateFlow(UsbLink())
    internal val usb = usbLink.asStateFlow()
    /** Whether the screen wants the tether watched while nothing is streaming. */
    internal var watchUsbWhenIdle: () -> Boolean = { false }
    private var wifiFallback: String? = null
    private var lastUsbAttempt = 0L
    private lateinit var wake: PowerManager.WakeLock
    // Wi-Fi power save delivers incoming audio in ~100 ms beacon bursts.
    private lateinit var wifiLocks: List<WifiManager.WifiLock>
    private val binder = LocalBinder()

    inner class LocalBinder : Binder() {
        val service
            get() = this@SessionService
    }

    private val devices =
        object : AudioDeviceCallback() {
            override fun onAudioDevicesRemoved(removed: Array<out AudioDeviceInfo>) {
                val ids = removed.map { it.id }.toSet()
                val next =
                    desired.copy(
                        mic = desired.mic && desired.input !in ids,
                        playback = desired.playback && desired.output !in ids,
                    )
                if (next != desired) update(next, UiText(R.string.message_device_disconnected))
            }
        }
    private val noisy =
        object : BroadcastReceiver() {
            override fun onReceive(context: Context?, intent: Intent?) {
                if (desired.playback)
                    update(
                        desired.copy(playback = false),
                        UiText(R.string.message_headphones_disconnected),
                    )
            }
        }

    override fun onCreate() {
        super.onCreate()
        audio = getSystemService(AudioManager::class.java)
        wake =
            getSystemService(PowerManager::class.java)
                .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "Litz:duplex")
                .apply { setReferenceCounted(false) }
        val wifi = applicationContext.getSystemService(WifiManager::class.java)
        wifiLocks =
            buildList {
                    if (wifi == null) return@buildList
                    if (Build.VERSION.SDK_INT >= 29)
                        add(
                            wifi.createWifiLock(
                                WifiManager.WIFI_MODE_FULL_LOW_LATENCY,
                                "Litz:low-latency",
                            )
                        )
                    // No-op from Android 14; covers older releases and background use.
                    if (Build.VERSION.SDK_INT < 34)
                        @Suppress("DEPRECATION")
                        add(
                            wifi.createWifiLock(
                                WifiManager.WIFI_MODE_FULL_HIGH_PERF,
                                "Litz:high-perf",
                            )
                        )
                }
                .onEach { it.setReferenceCounted(false) }
        if (Build.VERSION.SDK_INT >= 26)
            getSystemService(NotificationManager::class.java)
                .createNotificationChannel(
                    NotificationChannel(
                        CHANNEL,
                        getString(R.string.notification_channel),
                        NotificationManager.IMPORTANCE_LOW,
                    )
                )
        audio.registerAudioDeviceCallback(devices, Handler(mainLooper))
        if (Build.VERSION.SDK_INT >= 33)
            registerReceiver(
                noisy,
                IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY),
                RECEIVER_NOT_EXPORTED,
            )
        else
            @Suppress("DEPRECATION")
            registerReceiver(noisy, IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY))
        scope.launch {
            while (isActive) {
                delay(500)
                if (active)
                    mutex.withLock {
                        if (
                            desired.mic &&
                                checkSelfPermission(Manifest.permission.RECORD_AUDIO) !=
                                    PackageManager.PERMISSION_GRANTED
                        ) {
                            desired = desired.copy(mic = false)
                            applyLocked(UiText(R.string.message_microphone_permission_revoked))
                        } else
                            try {
                                publish(native { it.status() })
                            } catch (e: Exception) {
                                fail(e.text())
                            }
                    }
            }
        }
        scope.launch {
            var idleTurn = false
            while (isActive) {
                delay(1500)
                if (!desired.mic && !desired.playback) {
                    // Half as often while nothing is streaming.
                    idleTurn = !idleTurn
                    if (!idleTurn || !watchUsbWhenIdle()) continue
                }
                val route =
                    try {
                        withContext(Dispatchers.IO) {
                            val interfaces = UsbRoutes.interfaces()
                            val store = IdentityStore(this@SessionService)
                            val selected = store.selectedFingerprint()
                            val saved = store.saved().firstOrNull { it.fingerprint == selected }
                            Triple(
                                interfaces,
                                saved,
                                saved?.let {
                                    UsbMdns.resolve(
                                        it.fingerprint,
                                        it.endpoint.substringAfterLast(':').toIntOrNull()
                                            ?: defaultPort().toInt(),
                                        interfaces,
                                    )
                                },
                            )
                        }
                    } catch (error: CancellationException) {
                        throw error
                    } catch (_: Exception) {
                        continue
                    }
                val (interfaces, saved, found) = route
                val streaming = desired.mic || desired.playback
                if (!streaming && !watchUsbWhenIdle()) continue
                var endpoint = usbLink.value.endpoint
                if (saved != null && found != null) {
                    if (endpoint != (saved.fingerprint to found)) {
                        if (streaming && interfaces.none { it.subnet.contains(saved.endpoint) })
                            wifiFallback = saved.endpoint
                        endpoint = saved.fingerprint to found
                        lastUsbAttempt = 0L
                        if (!streaming) {
                            withContext(Dispatchers.IO) {
                                IdentityStore(this@SessionService)
                                    .updateAddress(saved.fingerprint, found)
                            }
                            address(found)
                        }
                    }
                    if (streaming && mutable.value.status?.connectedEndpoint != found) {
                        val now = SystemClock.elapsedRealtime()
                        if (now - lastUsbAttempt >= 5000L) {
                            lastUsbAttempt = now
                            address(found)
                        }
                    } else if (streaming && saved.endpoint != found)
                        withContext(Dispatchers.IO) {
                            IdentityStore(this@SessionService)
                                .updateAddress(saved.fingerprint, found)
                        }
                } else if (interfaces.isEmpty() && endpoint != null) {
                    endpoint = null
                    if (streaming && saved != null)
                        wifiFallback?.let { fallback ->
                            withContext(Dispatchers.IO) {
                                IdentityStore(this@SessionService)
                                    .updateAddress(saved.fingerprint, fallback)
                            }
                            address(fallback)
                        }
                }
                usbLink.value = UsbLink(interfaces.map { it.subnet }, endpoint)
            }
        }
    }

    override fun onBind(intent: Intent?): IBinder = binder

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            START -> {
                try {
                    val settings =
                        settingsFrom(intent.getBundleExtra("settings") ?: error("Missing settings"))
                    // Only the visible Activity issues START. No boot receiver,
                    // sticky restart, remote command, or notification can enable mic.
                    require(settings.mic || settings.playback)
                    if (settings.mic)
                        require(
                            checkSelfPermission(Manifest.permission.RECORD_AUDIO) ==
                                PackageManager.PERMISSION_GRANTED
                        )
                    promote(settings)
                    update(settings)
                } catch (e: Exception) {
                    fail(e.text())
                }
            }
            STOP -> update(desired.copy(mic = false, playback = false))
            MUTE -> if (active && desired.mic) update(desired.copy(muted = true))
        }
        return START_NOT_STICKY
    }

    private fun promote(s: Settings) {
        val types =
            (if (s.mic && Build.VERSION.SDK_INT >= 30)
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
            else 0) or (if (s.playback) ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK else 0)
        require(s.mic || s.playback)
        // Android 14+ checks microphone while-in-use permissions here.
        if (Build.VERSION.SDK_INT >= 29 && types != 0)
            startForeground(NOTIFICATION, notification(R.string.notification_connecting, s), types)
        else startForeground(NOTIFICATION, notification(R.string.notification_connecting, s))
        foreground = true
        foregroundTypes = if (Build.VERSION.SDK_INT >= 29) types else 0
    }

    internal fun update(settings: Settings, reason: UiText? = null) {
        scope.launch {
            mutex.withLock {
                val changingAudioPath = changesAudioPath(desired, settings)
                desired = settings
                localError = reason
                mutable.value =
                    mutable.value.copy(
                        settings = desired,
                        pending = true,
                        audioPending = changingAudioPath,
                        error = reason,
                        directionErrors = listOf(null, null),
                    )
                try {
                    applyLocked(reason)
                } catch (e: Exception) {
                    fail(e.text())
                }
            }
        }
    }

    private suspend fun applyLocked(reason: UiText?) {
        val generation = stopGeneration
        if (!desired.mic && !desired.playback) {
            val result = native { it.stop() }
            active = controlConnected
            releaseLocks()
            if (foreground) {
                removeForeground()
                foreground = false
            }
            mutable.value =
                SessionStatus(
                    desired,
                    result,
                    error = reason,
                    directionErrors =
                        if (reason != null) mutable.value.directionErrors else listOf(null, null),
                )
            stopSelf()
            return
        }
        promote(desired)
        // Listening deliberately mixes with other media, including YouTube.
        // Claiming AUDIOFOCUS_GAIN pauses the other app and invites Android to
        // mute us when it resumes. Hardware routing remains shared as well.
        openEngine()
        if (generation != stopGeneration) {
            desired = desired.copy(mic = false, playback = false)
            applyLocked(reason)
            return
        }
        active = true
        holdLocks()
        desired =
            desired.copy(
                rawSupported =
                    audio.getProperty(AudioManager.PROPERTY_SUPPORT_AUDIO_SOURCE_UNPROCESSED) ==
                        "true",
                headsetIds =
                    audio
                        .getDevices(AudioManager.GET_DEVICES_OUTPUTS)
                        .filter {
                            it.type in
                                listOf(
                                    AudioDeviceInfo.TYPE_WIRED_HEADSET,
                                    AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
                                    AudioDeviceInfo.TYPE_USB_HEADSET,
                                )
                        }
                        .map { it.id },
            )
        publish(native { it.set(desired) }, reason)
    }

    private suspend fun openEngine() =
        withContext(nativeDispatcher) {
            if (engine == null) {
                val identity =
                    IdentityStore(this@SessionService).readSelected()
                        ?: throw UserFacingException(UiText(R.string.message_pair_first))
                val opened = Engine(identity)
                synchronized(lifetime) { engine = opened }
            }
        }

    private suspend fun native(call: (Engine) -> Status): Status? =
        withContext(nativeDispatcher) { engine?.let(call) }

    private suspend fun closeEngine() =
        withContext(nativeDispatcher) {
            synchronized(lifetime) {
                engine?.close()
                engine = null
            }
        }

    private suspend fun publish(status: Status?, reason: UiText? = null) {
        status?.let { desired = it.desired }
        val error = reason ?: status?.error?.let { failureText(it) } ?: localError
        val reachable =
            status?.state in
                listOf(
                    SessionState.AUTHENTICATED,
                    SessionState.STREAMING,
                    SessionState.RECONNECTING,
                )
        val perDirection =
            listOf(status?.microphone, status?.listening).mapIndexed { index, direction ->
                direction?.error?.let { failureText(it, index) }
                    ?: error?.takeIf {
                        !reachable && if (index == 0) desired.mic else desired.playback
                    }
                    ?: mutable.value.directionErrors[index].takeIf { direction?.running != true }
            }
        mutable.value =
            SessionStatus(
                desired,
                status,
                pending = false,
                error = error,
                directionErrors = perDirection,
            )
        val hasAudio = status?.microphone?.running == true || status?.listening?.running == true
        val reconnecting = status?.state == SessionState.RECONNECTING
        if (!desired.mic && !desired.playback && foreground) {
            val movedAway = status?.owned != true && !status?.owner.isNullOrBlank()
            applyLocked(
                error ?: if (movedAway) null else UiText(R.string.message_directions_stopped)
            )
            return
        }
        if (!hasAudio && !reconnecting && error != null && foreground) {
            desired = desired.copy(mic = false, playback = false)
            applyLocked(error)
            return
        }
        if (hasAudio || reconnecting) {
            holdLocks()
        } else releaseLocks()
        val types =
            (if (desired.mic && Build.VERSION.SDK_INT >= 30)
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
            else 0) or
                (if (desired.playback) ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK else 0)
        if (
            foreground &&
                Build.VERSION.SDK_INT >= 29 &&
                foregroundTypes != types &&
                (desired.mic || desired.playback)
        )
            promote(desired)
        if (foreground)
            getSystemService(NotificationManager::class.java)
                .notify(NOTIFICATION, notification(stateText(status?.state), desired))
    }

    internal fun address(endpoint: String) {
        if (engine != null) control { it.address(endpoint) }
    }

    internal fun rememberWifiAddress(endpoint: String) {
        wifiFallback = endpoint
    }

    internal fun connect() = control { it.connect() }

    internal fun refreshRoutes() = control { it.refreshRoutes() }

    internal suspend fun forgetAndWait() {
        mutex.withLock { runCatching { native { it.forget() } } }
        disconnectAndWait()
    }

    internal suspend fun disconnectAndWait() {
        stopGeneration++
        synchronized(lifetime) { engine?.cancel() }
        mutex.withLock {
            desired = desired.copy(mic = false, playback = false)
            controlConnected = false
            applyLocked(null)
            closeEngine()
            active = false
            mutable.value = SessionStatus(desired)
        }
    }

    internal fun route(revision: ULong, route: Route) = control { it.route(revision, route) }

    private var pendingVoice: VoiceSettings? = null
    private var voiceJob: Job? = null

    internal fun voice(revision: ULong, voice: VoiceSettings) {
        localError = null
        mutable.value = mutable.value.copy(error = null)
        pendingVoice = voice
        if (voiceJob?.isActive == true) return
        voiceJob =
            scope.launch {
                while (pendingVoice != null) {
                    val next = pendingVoice ?: break
                    pendingVoice = null
                    mutex.withLock {
                        try {
                            val current = mutable.value.status?.desktopRoutes?.revision ?: revision
                            publish(native { it.voice(current, next) })
                        } catch (e: CancellationException) {
                            throw e
                        } catch (e: Exception) {
                            mutable.value = mutable.value.copy(error = e.text())
                        }
                    }
                }
            }
    }

    private fun control(call: (Engine) -> Status) {
        mutable.value = mutable.value.copy(pending = true, audioPending = false, error = null)
        scope.launch {
            mutex.withLock {
                try {
                    if (engine == null) {
                        openEngine()
                        val preferences = Preferences(this@SessionService).read()
                        native { it.set(preferences) }
                    }
                    controlConnected = true
                    active = true
                    publish(native(call))
                } catch (e: Exception) {
                    mutable.value = mutable.value.copy(pending = false, error = e.text())
                }
            }
        }
    }

    private fun holdLocks() {
        wake.acquire(60_000)
        wifiLocks.forEach { if (!it.isHeld) it.acquire() }
    }

    private fun releaseLocks() {
        if (wake.isHeld) wake.release()
        wifiLocks.forEach { if (it.isHeld) it.release() }
    }

    private fun fail(message: UiText) {
        stopGeneration++
        desired = desired.copy(mic = false, playback = false, muted = false)
        active = false
        controlConnected = false
        synchronized(lifetime) { engine?.cancel() }
        releaseLocks()
        if (foreground) {
            removeForeground()
            foreground = false
        }
        mutable.value = SessionStatus(settings = desired, error = message)
        scope.launch { mutex.withLock { closeEngine() } }
        stopSelf()
    }

    @StringRes
    private fun stateText(state: SessionState?): Int =
        when (state) {
            SessionState.RECONNECTING -> R.string.notification_reconnecting
            SessionState.STREAMING -> R.string.notification_streaming
            SessionState.AUTHENTICATED -> R.string.notification_connected
            SessionState.IDLE,
            null -> R.string.notification_idle
        }

    private fun notification(@StringRes state: Int, s: Settings): Notification {
        fun action(name: String, code: Int) =
            PendingIntent.getService(
                this,
                code,
                Intent(this, SessionService::class.java).setAction(name),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
        val open =
            PendingIntent.getActivity(
                this,
                0,
                Intent(this, MainActivity::class.java),
                PendingIntent.FLAG_IMMUTABLE,
            )
        val desktop = mutable.value.status?.desktop.orEmpty()
        val snapshot = Snapshot(mutable.value)
        val microphone =
            when {
                snapshot.running(0) -> R.string.diagnostics_on
                s.muted && snapshot.connected -> R.string.diagnostics_muted
                else -> R.string.diagnostics_off
            }
        val listen = if (snapshot.running(1)) R.string.diagnostics_on else R.string.diagnostics_off
        val builder =
            if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL)
            else @Suppress("DEPRECATION") Notification.Builder(this)
        return builder
            .setSmallIcon(R.drawable.ic_stat_litz)
            .setContentTitle(getString(R.string.notification_title, desktop))
            .setContentText(
                getString(
                    R.string.notification_text,
                    getString(state),
                    getString(microphone),
                    getString(listen),
                )
            )
            .setContentIntent(open)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .addAction(
                Notification.Action.Builder(
                        null,
                        getString(R.string.notification_stop),
                        action(STOP, 1),
                    )
                    .build()
            )
            .apply {
                if (s.mic && !s.muted)
                    addAction(
                        Notification.Action.Builder(
                                null,
                                getString(R.string.notification_mute),
                                action(MUTE, 2),
                            )
                            .build()
                    )
            }
            .build()
    }

    private fun removeForeground() {
        if (Build.VERSION.SDK_INT >= 24) stopForeground(STOP_FOREGROUND_REMOVE)
        else @Suppress("DEPRECATION") stopForeground(true)
    }

    override fun onDestroy() {
        active = false
        scope.cancel()
        audio.unregisterAudioDeviceCallback(devices)
        unregisterReceiver(noisy)
        releaseLocks()
        synchronized(lifetime) { engine?.cancel() }
        executor.execute {
            synchronized(lifetime) {
                engine?.close()
                engine = null
            }
        }
        executor.shutdown()
        super.onDestroy()
    }

    companion object {
        internal const val START = "audio.litz.START"
        internal const val STOP = "audio.litz.STOP"
        private const val MUTE = "audio.litz.MUTE"
        private const val CHANNEL = "session"
        private const val NOTIFICATION = 1
    }
}
