package audio.litz

import android.content.Context
import android.os.Build
import androidx.annotation.StringRes
import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import audio.litz.native.AudioInfo
import audio.litz.native.DesktopRoutes
import audio.litz.native.Route
import audio.litz.native.SessionState
import audio.litz.native.Status
import audio.litz.native.VoiceRuntime
import java.util.Locale

internal class Snapshot(session: SessionStatus) {
    val status: Status? = session.status
    private val directionErrors = session.directionErrors
    val state: SessionState = status?.state ?: SessionState.IDLE
    val connected = state == SessionState.AUTHENTICATED || state == SessionState.STREAMING
    val endpoint: String? = status?.connectedEndpoint
    val reconnectAllowed = status?.reconnectAllowed ?: true
    val revoked = status?.revoked == true
    val owner = status?.owner?.takeUnless { it.isBlank() }
    val generation: ULong = status?.ownershipGeneration ?: 0u
    val owned = status?.owned == true
    val inUse = connected && owner != null && !owned
    val routes: DesktopRoutes? = status?.desktopRoutes
    val audio: AudioInfo? = status?.audio
    val microphoneRms: Float? = status?.microphoneRms?.takeIf { running(0) && it.isFinite() }

    private fun direction(index: Int) = if (index == 0) status?.microphone else status?.listening

    fun running(index: Int) = direction(index)?.running == true

    fun error(index: Int): UiText? =
        direction(index)?.error?.let { failureText(it, index) } ?: directionErrors[index]
}

@Composable
internal fun routeDescription(route: Route?, inventory: DesktopRoutes): String =
    when (route) {
        null -> stringResource(R.string.unavailable)
        is Route.PhoneOnly -> PHONE_SPEAKER
        is Route.Mirror -> {
            val name = route.name
            if (name == null) {
                inventory.resolved?.let { resolved ->
                    val label =
                        inventory.endpoints.firstOrNull { it.name == resolved }?.label ?: resolved
                    stringResource(R.string.route_default, label)
                } ?: stringResource(R.string.route_default_unavailable)
            } else
                inventory.endpoints.firstOrNull { it.name == name }?.label
                    ?: stringResource(R.string.route_disappeared_choose)
        }
    }

/** The name the computer gives its virtual sink. */
private const val PHONE_SPEAKER = "Litz Phone Speaker"

internal data class DiagnosticSection(val title: String, val rows: List<Pair<String, String>>)

internal fun diagnostics(context: Context, ui: UiState, status: Snapshot): List<DiagnosticSection> {
    fun text(id: Int, vararg args: Any) = context.getString(id, *args)
    fun number(value: Number?, digits: Int = 0): String =
        value?.toDouble()?.takeIf { it.isFinite() }?.let { "%.${digits}f".format(Locale.ROOT, it) }
            ?: "?"
    val on = text(R.string.diagnostics_on)
    val off = text(R.string.diagnostics_off)
    val statusLabel = text(R.string.diagnostics_status)
    val details = status.status
    fun stream(
        rate: Int,
        lowLatency: Int,
        exclusive: Int,
        xruns: Int,
        @StringRes xrunLabel: Int,
    ): List<Pair<String, String>> {
        if (rate <= 0) return emptyList()
        return listOfNotNull(
            text(R.string.diagnostics_sample_rate) to text(R.string.unit_hertz, rate),
            text(R.string.diagnostics_latency_mode) to
                text(
                    if (lowLatency == 1) R.string.diagnostics_latency_low
                    else R.string.diagnostics_latency_normal
                ),
            text(R.string.diagnostics_sharing) to
                text(
                    if (exclusive == 1) R.string.diagnostics_sharing_exclusive
                    else R.string.diagnostics_sharing_shared
                ),
            (text(xrunLabel) to "$xruns").takeIf { xruns >= 0 },
        )
    }
    val audio = status.audio
    val input =
        audio
            ?.let {
                stream(
                    it.inputRate,
                    it.inputLowLatency,
                    it.inputExclusive,
                    it.inputXruns,
                    R.string.diagnostics_overruns,
                )
            }
            .orEmpty()
    val output =
        audio
            ?.let {
                stream(
                    it.outputRate,
                    it.outputLowLatency,
                    it.outputExclusive,
                    it.outputXruns,
                    R.string.diagnostics_underruns,
                )
            }
            .orEmpty()
    return buildList {
        add(
            DiagnosticSection(
                text(R.string.diagnostics_app),
                listOf(
                    text(R.string.diagnostics_version) to
                        "${BuildConfig.VERSION_NAME} (${BuildConfig.FLAVOR})",
                    text(R.string.diagnostics_android) to
                        "${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT})",
                    text(R.string.diagnostics_phone) to "${Build.MANUFACTURER} ${Build.MODEL}",
                ),
            )
        )
        add(
            DiagnosticSection(
                text(R.string.diagnostics_connection),
                listOfNotNull(
                    statusLabel to
                        text(
                            when {
                                status.inUse -> R.string.diagnostics_in_use
                                status.connected -> R.string.status_connected
                                status.state == SessionState.RECONNECTING ->
                                    R.string.diagnostics_reconnecting
                                else -> R.string.status_offline
                            }
                        ),
                    (text(R.string.diagnostics_round_trip) to
                            text(R.string.unit_milliseconds, number(details?.rttMs, 1)))
                        .takeIf { status.connected },
                ),
            )
        )
        if (!status.connected) return@buildList
        add(
            DiagnosticSection(
                text(R.string.microphone),
                if (!status.running(0)) listOf(statusLabel to off)
                else
                    listOfNotNull(
                        statusLabel to
                            if (ui.settings.muted) text(R.string.diagnostics_muted) else on,
                        text(R.string.recording_mode) to
                            recordingModeDescription(context, ui.settings.source, status),
                        status.routes?.voiceRuntime?.let {
                            text(R.string.diagnostics_voice_processing) to
                                text(voiceRuntimeName(it))
                        },
                        text(R.string.diagnostics_dropped_packets) to
                            number(details?.transportDrops?.toLong()),
                    ) + input,
            )
        )
        val listening = details?.listeningStats
        add(
            DiagnosticSection(
                text(R.string.listen),
                if (!status.running(1)) listOf(statusLabel to off)
                else
                    listOf(
                        statusLabel to on,
                        text(R.string.diagnostics_jitter_buffer) to
                            text(R.string.unit_milliseconds, number(listening?.bufferMs)),
                        text(R.string.diagnostics_late_packets) to
                            number(listening?.late?.toLong()),
                        text(R.string.diagnostics_concealed_packets) to
                            number(listening?.concealed?.toLong()),
                    ) +
                        output +
                        listOfNotNull(
                            audio?.let {
                                text(R.string.diagnostics_buffer) to
                                    text(R.string.unit_frames, "${it.outputBuffer}")
                            },
                            text(R.string.diagnostics_callback) to
                                text(
                                    R.string.unit_frames,
                                    number(details?.callbackFrames?.toLong()),
                                ),
                        ),
            )
        )
        val error = text(R.string.diagnostics_error)
        listOfNotNull(
                (text(R.string.microphone) to error).takeIf { status.error(0) != null },
                (text(R.string.listen) to error).takeIf { status.error(1) != null },
            )
            .takeIf { it.isNotEmpty() }
            ?.let { add(DiagnosticSection(text(R.string.diagnostics_errors), it)) }
    }
}

@StringRes
private fun voiceRuntimeName(runtime: VoiceRuntime): Int =
    when (runtime) {
        VoiceRuntime.OFF -> R.string.diagnostics_voice_off
        VoiceRuntime.ACTIVE -> R.string.diagnostics_voice_active
        VoiceRuntime.BYPASSED -> R.string.diagnostics_voice_bypassed
        VoiceRuntime.RECOVERING -> R.string.diagnostics_voice_recovering
    }

internal fun diagnosticText(sections: List<DiagnosticSection>): String =
    sections.joinToString("\n") { section ->
        section.title + section.rows.joinToString("") { (label, value) -> "\n  $label: $value" }
    }

internal fun recordingModeDescription(context: Context, selected: Int, status: Snapshot): String {
    fun title(value: Int) =
        microphoneSources.firstOrNull { it.value == value }?.let { context.getString(it.title) }
            ?: context.getString(R.string.recording_mode_unknown, value)
    val active = status.audio?.source?.takeIf { status.running(0) && it != 0 }
    return if (active == null || active == selected) title(selected)
    else context.getString(R.string.recording_mode_using, title(selected), title(active))
}

internal fun voiceRuntimeDescription(status: Snapshot): UiText? {
    if (!status.running(0)) return null
    return when (status.routes?.voiceRuntime) {
        VoiceRuntime.BYPASSED -> UiText(R.string.voice_bypassed)
        VoiceRuntime.RECOVERING -> UiText(R.string.voice_recovering)
        else -> null
    }
}
