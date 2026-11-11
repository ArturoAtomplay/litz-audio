package audio.litz

import android.content.Context
import android.os.Bundle
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.floatPreferencesKey
import androidx.datastore.preferences.core.intPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import audio.litz.native.Settings
import audio.litz.native.isCaptureSource
import kotlinx.coroutines.flow.first

private val Context.preferences by preferencesDataStore("preferences")

internal fun Settings.toBundle(): Bundle =
    Bundle().apply {
        putBoolean("mic", mic)
        putBoolean("playback", playback)
        putBoolean("muted", muted)
        putFloat("mic_gain", micGain)
        putFloat("playback_gain", playbackGain)
        putInt("input", input)
        putInt("source", source)
        putInt("output", output)
        putBoolean("reconnect", reconnect)
        takeover?.let { putLong("takeover", it.toLong()) }
        putBoolean("raw_supported", rawSupported)
        putIntArray("headset_ids", headsetIds.toIntArray())
    }

internal fun settingsFrom(bundle: Bundle): Settings =
    Settings(
        mic = bundle.getBoolean("mic"),
        playback = bundle.getBoolean("playback"),
        muted = bundle.getBoolean("muted"),
        micGain = bundle.getFloat("mic_gain", 1f),
        playbackGain = bundle.getFloat("playback_gain", 1f),
        input = bundle.getInt("input"),
        source = bundle.getInt("source"),
        output = bundle.getInt("output"),
        reconnect = bundle.getBoolean("reconnect", true),
        takeover =
            if (bundle.containsKey("takeover")) bundle.getLong("takeover").toULong() else null,
        rawSupported = bundle.getBoolean("raw_supported"),
        headsetIds = bundle.getIntArray("headset_ids")?.toList().orEmpty(),
    )

internal data class PreferenceDraft(
    val source: Int? = null,
    val micGain: Float? = null,
    val playbackGain: Float? = null,
    val reconnect: Boolean? = null,
) {
    fun withChanges(before: Settings, after: Settings) =
        copy(
            source = if (before.source != after.source) after.source else source,
            micGain = if (before.micGain != after.micGain) after.micGain else micGain,
            playbackGain =
                if (before.playbackGain != after.playbackGain) after.playbackGain else playbackGain,
            reconnect = if (before.reconnect != after.reconnect) after.reconnect else reconnect,
        )
}

internal fun mergePreferences(actual: Settings, draft: PreferenceDraft?): Settings =
    actual.copy(
        micGain = draft?.micGain ?: actual.micGain,
        playbackGain = draft?.playbackGain ?: actual.playbackGain,
        source = draft?.source ?: actual.source,
        reconnect = draft?.reconnect ?: actual.reconnect,
    )

internal class Preferences(private val context: Context) {
    private val source = intPreferencesKey("source")
    private val micGain = floatPreferencesKey("mic_gain")
    private val playbackGain = floatPreferencesKey("playback_gain")
    private val reconnect = booleanPreferencesKey("reconnect")

    suspend fun read(): Settings {
        val p = context.preferences.data.first()
        return Settings(
            micGain = p[micGain] ?: 1f,
            playbackGain = p[playbackGain]?.takeIf(Float::isFinite)?.coerceIn(0f, 2f) ?: 1f,
            reconnect = p[reconnect] ?: true,
            source = p[source]?.takeIf(::isCaptureSource) ?: 0,
        )
    }

    suspend fun writeChanged(before: Settings, after: Settings) {
        context.preferences.edit { p ->
            if (before.source != after.source) p[source] = after.source
            if (before.micGain != after.micGain) p[micGain] = after.micGain
            if (before.playbackGain != after.playbackGain) p[playbackGain] = after.playbackGain
            if (before.reconnect != after.reconnect) p[reconnect] = after.reconnect
        }
    }
}
