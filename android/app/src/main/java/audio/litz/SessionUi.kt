package audio.litz

import android.os.Build
import java.util.Locale
import org.json.JSONObject

internal class Snapshot(status: SessionStatus) {
    private val directionErrors = status.directionErrors
    private val json = runCatching { JSONObject(status.json) }.getOrDefault(JSONObject())
    val state: String = json.optString("state", "Idle")
    val connected = state == "Authenticated" || state == "Streaming"
    val reconnectAllowed = json.optBoolean("reconnect_allowed", true)
    val owner = json.optString("owner").takeUnless { it.isBlank() || it == "null" }
    val generation = json.optLong("ownership_generation")
    val owned = json.optBoolean("owned")
    val inUse = connected && owner != null && !owned
    val routes = json.optJSONObject("desktop_routes")
    val audio = json.optJSONObject("audio")
    val applied = json.optJSONObject("applied")
    val microphoneRms =
        json.optDouble("microphone_rms", Double.NaN).takeIf { running(0) && it.isFinite() }

    fun running(index: Int) = json.optJSONArray("running")?.optBoolean(index) == true

    fun error(index: Int): String? =
        json.optJSONArray("direction_errors")?.optString(index)?.takeUnless {
            it.isBlank() || it == "null"
        } ?: directionErrors[index]

    fun metric(key: String) =
        if (json.isNull(key)) "unavailable" else json.opt(key)?.toString() ?: "unavailable"
}

internal fun routeDescription(route: Any?, inventory: JSONObject): String {
    if (route == null || route == JSONObject.NULL) return "unavailable"
    if (route == "PhoneOnly") return "Litz Phone Speaker"
    val mirror = (route as? JSONObject)?.optJSONObject("Mirror") ?: return "unavailable"
    if (mirror.isNull("name")) {
        val resolved =
            inventory.optString("resolved").takeUnless { it.isBlank() || it == "null" }
                ?: return "Default output · unavailable"
        val endpoints = inventory.optJSONArray("endpoints")
        val label =
            (0 until (endpoints?.length() ?: 0))
                .map { endpoints!!.getJSONObject(it) }
                .firstOrNull { it.optString("name") == resolved }
                ?.optString("label") ?: resolved
        return "Default · $label"
    }
    val endpoints = inventory.optJSONArray("endpoints") ?: return "Endpoint disappeared"
    for (i in 0 until endpoints.length()) if (
        endpoints.getJSONObject(i).optString("name") == mirror.optString("name")
    )
        return endpoints.getJSONObject(i).optString("label")
    return "Endpoint disappeared · choose another source"
}

internal data class DiagnosticSection(val title: String, val rows: List<Pair<String, String>>)

internal fun diagnostics(ui: UiState, status: Snapshot): List<DiagnosticSection> {
    fun number(key: String, digits: Int = 0): String =
        status.metric(key).toDoubleOrNull()?.let { "%.${digits}f".format(Locale.ROOT, it) } ?: "?"
    val audio = status.audio
    fun stream(side: String): List<Pair<String, String>> {
        if (audio == null || audio.optInt("${side}_rate") <= 0) return emptyList()
        val xruns = audio.optInt("${side}_xruns", -1)
        return listOfNotNull(
            "Sample rate" to "${audio.optInt("${side}_rate")} Hz",
            "Latency mode" to if (audio.optInt("${side}_low_latency") == 1) "Low" else "Normal",
            "Sharing" to if (audio.optInt("${side}_exclusive") == 1) "Exclusive" else "Shared",
            ((if (side == "input") "Overruns" else "Underruns") to "$xruns").takeIf { xruns >= 0 },
        )
    }
    return buildList {
        add(
            DiagnosticSection(
                "App",
                listOf(
                    "Version" to "${BuildConfig.VERSION_NAME} (${BuildConfig.FLAVOR})",
                    "Android" to "${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT})",
                    "Phone" to "${Build.MANUFACTURER} ${Build.MODEL}",
                ),
            )
        )
        add(
            DiagnosticSection(
                "Connection",
                listOfNotNull(
                    "Status" to
                        when {
                            status.inUse -> "In use by another phone"
                            status.connected -> "Connected"
                            status.state == "Reconnecting" -> "Reconnecting"
                            else -> "Offline"
                        },
                    ("Round trip" to "${number("rtt_ms", 1)} ms").takeIf { status.connected },
                ),
            )
        )
        if (!status.connected) return@buildList
        add(
            DiagnosticSection(
                "Microphone",
                if (!status.running(0)) listOf("Status" to "Off")
                else
                    listOfNotNull(
                        "Status" to if (ui.settings.muted) "Muted" else "On",
                        "Recording mode" to recordingModeDescription(ui.settings.source, status),
                        status.routes
                            ?.optString("voice_runtime")
                            ?.takeIf { it.isNotBlank() }
                            ?.let { "Voice processing" to it.replaceFirstChar(Char::uppercase) },
                        "Dropped packets" to number("transport_drops"),
                    ) + stream("input"),
            )
        )
        add(
            DiagnosticSection(
                "Listen",
                if (!status.running(1)) listOf("Status" to "Off")
                else
                    listOf(
                        "Status" to "On",
                        "Jitter buffer" to "${number("jitter_ms")} ms",
                        "Late packets" to number("late_packets"),
                        "Concealed packets" to number("concealed_frames"),
                    ) +
                        stream("output") +
                        listOfNotNull(
                            audio?.let { "Buffer" to "${it.optInt("output_buffer")} frames" },
                            "Callback" to "${number("callback_frames")} frames",
                        ),
            )
        )
        listOfNotNull(
                ("Microphone" to "Error").takeIf { status.error(0) != null },
                ("Listen" to "Error").takeIf { status.error(1) != null },
            )
            .takeIf { it.isNotEmpty() }
            ?.let { add(DiagnosticSection("Errors", it)) }
    }
}

internal fun diagnosticText(sections: List<DiagnosticSection>): String =
    sections.joinToString("\n") { section ->
        section.title + section.rows.joinToString("") { (label, value) -> "\n  $label: $value" }
    }

internal fun recordingModeDescription(selected: Int, status: Snapshot): String {
    fun title(value: Int) =
        microphoneSources.firstOrNull { it.value == value }?.title ?: "Unknown ($value)"
    val active = status.audio?.optInt("source")?.takeIf { status.running(0) && it != 0 }
    return if (active == null || active == selected) title(selected)
    else "${title(selected)} · using ${title(active)}"
}

internal fun voiceRuntimeDescription(status: Snapshot): String? {
    if (!status.running(0)) return null
    return when (status.routes?.optString("voice_runtime")) {
        "bypassed" ->
            "Voice effects temporarily unavailable. Audio continues; retrying automatically."
        "recovering" -> "Restoring voice effects. Audio continues without effects."
        else -> null
    }
}
