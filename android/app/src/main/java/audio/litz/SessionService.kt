package audio.litz

import android.Manifest
import android.app.*
import android.content.*
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.media.*
import android.net.wifi.WifiManager
import android.os.*
import androidx.compose.runtime.Immutable
import java.util.concurrent.Executors
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.json.JSONObject

@Immutable
internal data class SessionStatus(
    val settings: Settings = Settings(),
    val json: String = "{}",
    val pending: Boolean = false,
    val error: String? = null,
    val directionErrors: List<String?> = listOf(null, null),
    val audioPending: Boolean = false,
)

class SessionService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val executor = Executors.newSingleThreadExecutor { Thread(it, "litz-control") }
    private val nativeDispatcher = executor.asCoroutineDispatcher()
    private val mutex = Mutex()
    private var handle =
        0L // command access on nativeDispatcher; lifetime guarded for urgent cancel
    private val lifetime = Any()
    private var stopGeneration = 0
    private val mutable = MutableStateFlow(SessionStatus())
    internal val status = mutable.asStateFlow()
    private var desired = Settings()
    private lateinit var audio: AudioManager
    private var foreground = false
    private var foregroundTypes = 0
    private var localError: String? = null
    private var active = false
    private var controlConnected = false
    private var usbEndpoint: String? = null
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
                if (next != desired)
                    update(next, "Selected audio device disconnected; direction stopped")
            }
        }
    private val noisy =
        object : BroadcastReceiver() {
            override fun onReceive(context: Context?, intent: Intent?) {
                if (desired.playback)
                    update(
                        desired.copy(playback = false),
                        "Headphones disconnected — tap Listen to use your phone speaker",
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
                        "Audio session",
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
                            applyLocked("Microphone permission revoked")
                        } else
                            try {
                                publish(native("{\"op\":\"status\"}"))
                            } catch (e: Exception) {
                                fail(e.message ?: "Native session unavailable")
                            }
                    }
            }
        }
        scope.launch {
            while (isActive) {
                delay(1500)
                if (!desired.mic && !desired.playback) continue
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
                                        it.endpoint.substringAfterLast(':').toIntOrNull() ?: 47165,
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
                if (!desired.mic && !desired.playback) continue
                if (saved == null) continue
                if (found != null) {
                    if (found != usbEndpoint) {
                        if (interfaces.none { it.subnet.contains(saved.endpoint) })
                            wifiFallback = saved.endpoint
                        usbEndpoint = found
                        lastUsbAttempt = 0L
                    }
                    if (JSONObject(mutable.value.json).optString("connected_endpoint") != found) {
                        val now = SystemClock.elapsedRealtime()
                        if (now - lastUsbAttempt >= 5000L) {
                            lastUsbAttempt = now
                            address(found)
                        }
                    } else if (saved.endpoint != found)
                        withContext(Dispatchers.IO) {
                            IdentityStore(this@SessionService)
                                .updateAddress(saved.fingerprint, found)
                        }
                } else if (interfaces.isEmpty() && usbEndpoint != null) {
                    usbEndpoint = null
                    wifiFallback?.let { fallback ->
                        withContext(Dispatchers.IO) {
                            IdentityStore(this@SessionService)
                                .updateAddress(saved.fingerprint, fallback)
                        }
                        address(fallback)
                    }
                }
            }
        }
    }

    override fun onBind(intent: Intent?): IBinder = binder

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            START -> {
                try {
                    val settings =
                        Settings.from(
                            JSONObject(
                                intent.getStringExtra("settings") ?: error("Missing settings")
                            )
                        )
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
                    fail(e.message ?: "Foreground start not allowed")
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
            startForeground(NOTIFICATION, notification("Connecting", s), types)
        else startForeground(NOTIFICATION, notification("Connecting", s))
        foreground = true
        foregroundTypes = if (Build.VERSION.SDK_INT >= 29) types else 0
    }

    internal fun update(settings: Settings, reason: String? = null) {
        scope.launch {
            mutex.withLock {
                val changingAudioPath = desired.changesAudioPath(settings)
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
                    fail(e.message ?: "Session failed")
                }
            }
        }
    }

    private suspend fun applyLocked(reason: String?) {
        val generation = stopGeneration
        if (!desired.mic && !desired.playback) {
            val result = native("{\"op\":\"stop\"}")
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
        withContext(nativeDispatcher) {
            if (handle == 0L) {
                val identity =
                    IdentityStore(this@SessionService).readSelected()
                        ?: error("Pair a computer first")
                synchronized(lifetime) { handle = Native.open(identity) }
                check(handle != 0L) { "Native engine could not start" }
            }
        }
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
        val result =
            native(JSONObject().put("op", "set").put("settings", desired.json()).toString())
        publish(result, reason)
    }

    private suspend fun native(command: String): String =
        withContext(nativeDispatcher) {
            if (handle == 0L) "{}" else Native.command(handle, command)
        }

    private suspend fun publish(result: String, reason: String? = null) {
        val j = JSONObject(result)
        j.optJSONObject("desired")?.let { desired = Settings.from(it) }
        val error =
            reason ?: j.optString("error").takeUnless { it.isBlank() || it == "null" } ?: localError
        val errors = j.optJSONArray("direction_errors")
        val perDirection =
            (0..1).map { index ->
                errors?.optString(index)?.takeUnless { it.isBlank() || it == "null" }
                    ?: error?.takeIf {
                        !j.optString("state").let { state ->
                            state == "Authenticated" ||
                                state == "Streaming" ||
                                state == "Reconnecting"
                        } && if (index == 0) desired.mic else desired.playback
                    }
                    ?: mutable.value.directionErrors[index].takeIf {
                        j.optJSONArray("running")?.optBoolean(index) != true
                    }
            }
        mutable.value =
            SessionStatus(
                desired,
                result,
                pending = false,
                error = error,
                directionErrors = perDirection,
            )
        val running = j.optJSONArray("running")
        val hasAudio = running?.optBoolean(0) == true || running?.optBoolean(1) == true
        val reconnecting = j.optString("state") == "Reconnecting"
        if (!desired.mic && !desired.playback && foreground) {
            val movedAway =
                !j.optBoolean("owned") &&
                    j.optString("owner").let { it.isNotBlank() && it != "null" }
            applyLocked(error ?: if (movedAway) null else "Audio directions stopped")
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
                .notify(NOTIFICATION, notification(j.optString("state", "Idle"), desired))
    }

    internal fun address(endpoint: String) {
        if (handle != 0L)
            control(JSONObject().put("op", "address").put("endpoint", endpoint).toString())
    }

    internal fun rememberWifiAddress(endpoint: String) {
        wifiFallback = endpoint
    }

    internal fun connect() = control("{\"op\":\"connect\"}")

    internal fun refreshRoutes() = control("{\"op\":\"refresh_routes\"}")

    internal suspend fun forgetAndWait() {
        mutex.withLock { if (handle != 0L) runCatching { native("{\"op\":\"forget\"}") } }
        disconnectAndWait()
    }

    internal fun disconnect() {
        scope.launch { disconnectAndWait() }
    }

    internal suspend fun disconnectAndWait() {
        stopGeneration++
        synchronized(lifetime) { if (handle != 0L) Native.cancel(handle) }
        mutex.withLock {
            desired = desired.copy(mic = false, playback = false)
            controlConnected = false
            applyLocked(null)
            withContext(nativeDispatcher) {
                synchronized(lifetime) {
                    if (handle != 0L) {
                        Native.close(handle)
                        handle = 0L
                    }
                }
            }
            active = false
            mutable.value = SessionStatus(desired)
        }
    }

    internal fun route(revision: Long, route: Any) =
        control(
            JSONObject()
                .put("op", "route")
                .put("expected_revision", revision)
                .put("route", route)
                .toString()
        )

    private var pendingVoice: JSONObject? = null
    private var voiceJob: Job? = null

    internal fun voice(revision: Long, voice: JSONObject) {
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
                            val current =
                                Snapshot(mutable.value).routes?.optLong("revision") ?: revision
                            publish(
                                native(
                                    JSONObject()
                                        .put("op", "voice")
                                        .put("expected_revision", current)
                                        .put("voice", next)
                                        .toString()
                                )
                            )
                        } catch (e: CancellationException) {
                            throw e
                        } catch (e: Exception) {
                            mutable.value =
                                mutable.value.copy(
                                    error = e.message ?: "Settings could not be saved"
                                )
                        }
                    }
                }
            }
    }

    private fun control(command: String) {
        mutable.value = mutable.value.copy(pending = true, audioPending = false, error = null)
        scope.launch {
            mutex.withLock {
                try {
                    val opening = handle == 0L
                    withContext(nativeDispatcher) {
                        if (handle == 0L) {
                            val identity =
                                IdentityStore(this@SessionService).readSelected()
                                    ?: error("Pair a computer first")
                            synchronized(lifetime) { handle = Native.open(identity) }
                            check(handle != 0L) { "Could not open the native engine" }
                        }
                    }
                    if (opening) {
                        val preferences = Preferences(this@SessionService).read()
                        native(
                            JSONObject()
                                .put("op", "set")
                                .put("settings", preferences.json())
                                .toString()
                        )
                    }
                    controlConnected = true
                    active = true
                    publish(native(command))
                } catch (e: Exception) {
                    mutable.value =
                        mutable.value.copy(
                            pending = false,
                            error = e.message ?: "Could not connect",
                        )
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

    private fun fail(message: String) {
        stopGeneration++
        desired = desired.copy(mic = false, playback = false, muted = false)
        active = false
        controlConnected = false
        synchronized(lifetime) { if (handle != 0L) Native.cancel(handle) }
        releaseLocks()
        if (foreground) {
            removeForeground()
            foreground = false
        }
        mutable.value = SessionStatus(settings = desired, error = message)
        scope.launch {
            mutex.withLock {
                withContext(nativeDispatcher) {
                    synchronized(lifetime) {
                        if (handle != 0L) {
                            Native.close(handle)
                            handle = 0L
                        }
                    }
                }
            }
        }
        stopSelf()
    }

    private fun notification(state: String, s: Settings): Notification {
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
        val desktop =
            runCatching { JSONObject(mutable.value.json).optString("desktop") }.getOrDefault("")
        val builder =
            if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL)
            else @Suppress("DEPRECATION") Notification.Builder(this)
        return builder
            .setSmallIcon(R.drawable.ic_stat_litz)
            .setContentTitle("Litz Audio · $desktop")
            .setContentText(
                "$state · mic ${if (Snapshot(mutable.value).running(0)) "running" else if (s.muted && Snapshot(mutable.value).connected) "muted" else "off"} · listen ${if (Snapshot(mutable.value).running(1)) "running" else "off"}"
            )
            .setContentIntent(open)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .addAction(Notification.Action.Builder(null, "Stop", action(STOP, 1)).build())
            .apply {
                if (s.mic && !s.muted)
                    addAction(
                        Notification.Action.Builder(null, "Mute mic", action(MUTE, 2)).build()
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
        synchronized(lifetime) { if (handle != 0L) Native.cancel(handle) }
        executor.execute {
            synchronized(lifetime) {
                if (handle != 0L) {
                    Native.close(handle)
                    handle = 0L
                }
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
