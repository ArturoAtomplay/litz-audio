package audio.litz

import android.content.Context
import androidx.compose.runtime.Immutable
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.floatPreferencesKey
import androidx.datastore.preferences.core.intPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import kotlinx.coroutines.flow.first
import org.json.JSONObject

private val Context.preferences by preferencesDataStore("preferences")

@Immutable
internal data class Settings(
    val mic: Boolean = false,
    val playback: Boolean = false,
    val muted: Boolean = false,
    val micGain: Float = 1f,
    val playbackGain: Float = 1f,
    val input: Int = 0,
    val output: Int = 0,
    val reconnect: Boolean = true,
    val source: Int = 0,
    val takeover: Long? = null,
    val rawSupported: Boolean = false,
    val headsetIds: List<Int> = emptyList(),
) {
    fun json() =
        JSONObject()
            .put("mic", mic)
            .put("playback", playback)
            .put("muted", muted)
            .put("mic_gain", micGain)
            .put("playback_gain", playbackGain)
            .put("takeover", takeover ?: JSONObject.NULL)
            .put("raw_supported", rawSupported)
            .put("headset_ids", org.json.JSONArray(headsetIds))
            .put("source", source)
            .put("input", input)
            .put("output", output)
            .put("reconnect", reconnect)

    companion object {
        fun from(j: JSONObject) =
            Settings(
                j.getBoolean("mic"),
                j.getBoolean("playback"),
                j.getBoolean("muted"),
                j.getDouble("mic_gain").toFloat(),
                j.getDouble("playback_gain").toFloat(),
                j.getInt("input"),
                j.getInt("output"),
                j.getBoolean("reconnect"),
                j.optInt("source", 0),
                if (j.isNull("takeover")) null else j.getLong("takeover"),
                j.optBoolean("raw_supported"),
                j.optJSONArray("headset_ids")?.let { a ->
                    (0 until a.length()).map { a.getInt(it) }
                } ?: emptyList(),
            )
    }
}

internal fun Settings.changesAudioPath(next: Settings): Boolean =
    mic != next.mic ||
        playback != next.playback ||
        muted != next.muted ||
        source != next.source ||
        input != next.input ||
        output != next.output

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

// A draft contains only touched preferences and cannot resurrect stopped directions.
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
        // Never persist enabled directions, mute authorization, or ephemeral device IDs.
        return Settings(
            micGain = p[micGain] ?: 1f,
            playbackGain = p[playbackGain]?.takeIf(Float::isFinite)?.coerceIn(0f, 2f) ?: 1f,
            reconnect = p[reconnect] ?: true,
            source = p[source]?.takeIf { it in listOf(0, 1, 5, 6, 7, 9, 10) } ?: 0,
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
