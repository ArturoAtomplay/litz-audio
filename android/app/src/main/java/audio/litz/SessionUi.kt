package audio.litz

import org.json.JSONObject

/** One low-rate engine snapshot; requested settings never stand in for running. */
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

    fun direction(index: Int, status: SessionStatus): String {
        val requested = if (index == 0) status.settings.mic else status.settings.playback
        if (status.pending && status.audioPending && requested != running(index))
            return if (requested) "Starting…" else "Stopping…"
        if (state == "Reconnecting" && requested) return "Reconnecting…"
        if (error(index) != null) return "Error"
        if (running(index)) return "Running"
        if (index == 0 && requested && applied?.optBoolean("muted") == true && connected)
            return "Muted"
        if (requested && status.error != null) return "Error"
        return "Off"
    }

    val connection: String
        get() =
            when {
                state == "Reconnecting" -> "Reconnecting"
                connected -> "Connected"
                else -> "Disconnected"
            }
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

internal fun diagnosticReport(ui: UiState, status: Snapshot): String = buildString {
    appendLine("State: ${ui.lifecycle}")
    appendLine(
        "Connection: ${if (ui.session.error != null && !status.connected) "Error" else status.connection}"
    )
    appendLine("Profile: ${status.metric("profile")}")
    appendLine("Stream rate (Hz): ${status.metric("stream_rate")}")
    appendLine("Packet duration (ms): ${status.metric("frame_ms")}")
    appendLine("Microphone channels: ${status.metric("microphone_channels")}")
    appendLine("Listen channels: ${status.metric("playback_channels")}")
    listOf(
            "RTT (ms)" to "rtt_ms",
            "Jitter buffer (ms)" to "jitter_ms",
            "Late packets" to "late_packets",
            "PLC frames" to "concealed_frames",
            "Underruns" to "underruns",
            "Transport drops" to "transport_drops",
        )
        .forEach { (label, key) -> appendLine("$label: ${status.metric(key)}") }
    appendLine("Microphone: ${status.direction(0, ui.session)}")
    appendLine("Listen: ${status.direction(1, ui.session)}")
    val audio = status.audio
    listOf(
            "Input rate (Hz)" to "input_rate",
            "Output rate (Hz)" to "output_rate",
            "Input buffer (frames)" to "input_buffer",
            "Output buffer (frames)" to "output_buffer",
        )
        .forEach { (label, key) ->
            appendLine(
                "$label: ${if (audio == null || audio.isNull(key) || audio.optInt(key) == 0) "unavailable" else audio.opt(key)}"
            )
        }
    // Deliberately do not include arbitrary error strings, endpoint names, IPs,
    // fingerprints, identity JSON or pairing material in the copied report.
    appendLine(
        "Microphone route error: ${if (status.error(0) == null) "none reported" else "present (details in app)"}"
    )
    appendLine(
        "Playback route error: ${if (status.error(1) == null) "none reported" else "present (details in app)"}"
    )
    appendLine("Windows microphone requires VB-CABLE on the computer")
    appendLine(
        "Voice effects: ${status.routes?.optString("voice_runtime", "unavailable") ?: "unavailable"}"
    )
    appendLine("Capture source ID: ${audio?.optInt("source")?.takeIf { it != 0 } ?: "unavailable"}")
}

internal fun recordingModeDescription(status: Snapshot): String {
    if (!status.running(0)) return "How Android captures your voice"
    val source = status.audio?.optInt("source") ?: return "Active mode unavailable"
    val title =
        microphoneSources.firstOrNull { it.value == source }?.title ?: "Unknown mode ($source)"
    return "Active: $title · Android may add device processing"
}

internal fun voiceRuntimeDescription(status: Snapshot): String? {
    if (!status.running(0)) return null
    return when (status.routes?.optString("voice_runtime")) {
        "active" -> "Voice effects active"
        "off" -> "Voice effects off"
        "bypassed" ->
            "Voice effects temporarily unavailable. Audio continues; retrying automatically."
        "recovering" -> "Restoring voice effects. Audio continues without effects."
        else -> null
    }
}
