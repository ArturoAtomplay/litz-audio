package audio.litz

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import kotlin.math.log10
import kotlin.math.roundToInt
import kotlinx.coroutines.delay
import org.json.JSONArray
import org.json.JSONObject

@Immutable
internal data class VoicePreferences(
    val suppression: String = "none",
    val level: Boolean = false,
    val eq: List<Int> = listOf(0, 0, 0),
    val sensitivity: String = "off",
    val threshold: Int = -40,
) {
    fun json(): JSONObject =
        JSONObject()
            .put("suppression", suppression)
            .put("auto_level", level)
            .put("eq_db", JSONArray(eq))
            .apply {
                when (sensitivity) {
                    "automatic" -> put("input_sensitivity", "automatic")
                    "manual" -> put("input_sensitivity", JSONObject().put("manual", threshold))
                }
            }

    companion object {
        fun from(json: JSONObject) =
            VoicePreferences(
                json.optString("suppression", "none"),
                json.optBoolean("auto_level"),
                List(3) { json.optJSONArray("eq_db")?.optInt(it)?.coerceIn(-12, 12) ?: 0 },
                when (val value = json.opt("input_sensitivity")) {
                    "automatic" -> "automatic"
                    is JSONObject -> "manual"
                    else -> "off"
                },
                json.optJSONObject("input_sensitivity")?.optInt("manual", -40)?.coerceIn(-60, -20)
                    ?: -40,
            )
    }
}

// Keep only locally edited fields while acknowledgements for earlier edits arrive.
internal fun voiceChangedFields(before: VoicePreferences, after: VoicePreferences): Set<Int> =
    buildSet {
        if (before.suppression != after.suppression) add(0)
        if (before.level != after.level) add(2)
        for (index in 0..2) if (before.eq[index] != after.eq[index]) add(index + 3)
        if (before.sensitivity != after.sensitivity) add(7)
        if (before.threshold != after.threshold) add(8)
    }

internal fun rebaseVoiceDraft(
    actual: VoicePreferences,
    draft: VoicePreferences,
    dirty: Set<Int>,
): Pair<VoicePreferences, Set<Int>> {
    val pending =
        dirty.filterTo(mutableSetOf()) { field ->
            when (field) {
                0 -> actual.suppression != draft.suppression
                2 -> actual.level != draft.level
                7 -> actual.sensitivity != draft.sensitivity
                8 -> actual.threshold != draft.threshold
                else -> actual.eq[field - 3] != draft.eq[field - 3]
            }
        }
    return VoicePreferences(
        suppression = if (0 in pending) draft.suppression else actual.suppression,
        level = if (2 in pending) draft.level else actual.level,
        eq = List(3) { if (it + 3 in pending) draft.eq[it] else actual.eq[it] },
        sensitivity = if (7 in pending) draft.sensitivity else actual.sensitivity,
        threshold = if (8 in pending) draft.threshold else actual.threshold,
    ) to pending
}

@Composable
internal fun VoiceControls(
    computer: String?,
    revision: Long,
    saved: VoicePreferences,
    enabled: Boolean,
    supported: List<String> = listOf("none", "rnnoise", "deep_filter_net"),
    failure: String? = null,
    inputRms: Double? = null,
    apply: (Long, JSONObject) -> Unit,
) {
    val actual = saved
    var draft by remember(computer) { mutableStateOf(actual) }
    var dirty by remember(computer) { mutableStateOf(emptySet<Int>()) }
    var sliderPending by remember(computer) { mutableStateOf(false) }
    val latestApply by rememberUpdatedState(apply)
    val latestRevision by rememberUpdatedState(revision)
    LaunchedEffect(failure) {
        if (failure != null) {
            draft = actual
            dirty = emptySet()
            sliderPending = false
        }
    }
    LaunchedEffect(actual, enabled) {
        if (!enabled) {
            draft = actual
            dirty = emptySet()
        } else {
            val (rebased, pending) = rebaseVoiceDraft(actual, draft, dirty)
            draft = rebased
            dirty = pending
        }
    }
    fun send(next: VoicePreferences) {
        dirty = dirty + voiceChangedFields(draft, next)
        draft = next
        latestApply(latestRevision, next.json())
    }
    LaunchedEffect(draft, sliderPending) {
        if (sliderPending && enabled) {
            delay(100)
            latestApply(latestRevision, draft.json())
        }
    }
    OutlinedCard(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Voice processing", style = MaterialTheme.typography.titleLarge)
            Text(
                if (enabled) "Fine-tune your microphone sound on the computer."
                else "Voice controls are temporarily unavailable.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            var expanded by remember { mutableStateOf(false) }
            val algorithms =
                listOf(
                        "none" to "None",
                        "rnnoise" to "RNNoise",
                        "deep_filter_net" to "DeepFilterNet",
                    )
                    .filter { it.first in supported }
            OutlinedCard(
                onClick = { expanded = true },
                enabled = enabled,
                modifier =
                    Modifier.fillMaxWidth().semantics { contentDescription = "Noise suppression" },
                shape = RoundedCornerShape(16.dp),
            ) {
                SettingValue(
                    "Noise suppression",
                    algorithms.firstOrNull { it.first == draft.suppression }?.second
                        ?: "Unavailable",
                    enabled = enabled,
                )
            }
            if (expanded) {
                AudioChoiceSheet("Noise suppression", dismiss = { expanded = false }) {
                    algorithms.forEach { (value, label) ->
                        AudioChoiceRow(
                            label,
                            selected = draft.suppression == value,
                            enabled = enabled,
                            description =
                                when (value) {
                                    "deep_filter_net" ->
                                        "Stronger reduction, but difficult noise may alter your voice."
                                    else -> null
                                },
                        ) {
                            expanded = false
                            send(draft.copy(suppression = value))
                        }
                    }
                }
            }

            Text(
                "Use when speaker audio leaks into the phone microphone. May reduce sustained speech; leave off when echo is not a problem.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            VoiceToggle("Automatic level", draft.level, enabled) { send(draft.copy(level = it)) }
            inputRms?.let { rms ->
                val db = (20.0 * log10(rms.coerceAtLeast(0.000001))).coerceIn(-60.0, 0.0)
                Text("Input level: ${db.roundToInt()} dBFS")
                LinearProgressIndicator(
                    progress = { ((db + 60.0) / 60.0).toFloat() },
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            var sensitivityExpanded by remember { mutableStateOf(false) }
            OutlinedCard(
                onClick = { sensitivityExpanded = true },
                enabled = enabled,
                modifier =
                    Modifier.fillMaxWidth().semantics { contentDescription = "Input sensitivity" },
                shape = RoundedCornerShape(16.dp),
            ) {
                SettingValue(
                    "Input sensitivity",
                    when (draft.sensitivity) {
                        "automatic" -> "Automatic"
                        "manual" -> "Manual"
                        else -> "Off"
                    },
                    enabled = enabled,
                )
            }
            if (sensitivityExpanded) {
                AudioChoiceSheet("Input sensitivity", dismiss = { sensitivityExpanded = false }) {
                    listOf("off" to "Off", "automatic" to "Automatic", "manual" to "Manual")
                        .forEach { (value, label) ->
                            AudioChoiceRow(
                                label,
                                selected = draft.sensitivity == value,
                                enabled = enabled,
                            ) {
                                sensitivityExpanded = false
                                send(draft.copy(sensitivity = value))
                            }
                        }
                }
            }
            if (draft.sensitivity == "manual") {
                Column {
                    Row(Modifier.fillMaxWidth()) {
                        Text("Voice threshold", Modifier.weight(1f))
                        Text("${draft.threshold} dBFS")
                    }
                    Slider(
                        value = draft.threshold.toFloat(),
                        onValueChange = { value ->
                            dirty = dirty + 8
                            sliderPending = true
                            draft = draft.copy(threshold = value.roundToInt())
                        },
                        onValueChangeFinished = {
                            sliderPending = false
                            send(draft)
                        },
                        valueRange = -60f..-20f,
                        steps = 39,
                        enabled = enabled,
                        modifier = Modifier.semantics { contentDescription = "Voice threshold" },
                    )
                    Text(
                        "Lower values pick up quieter speech; higher values block more background sound.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            HorizontalDivider()
            Text("Equalizer", style = MaterialTheme.typography.titleMedium)
            listOf("Low", "Mid", "High").forEachIndexed { index, label ->
                Column {
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Text(
                            label,
                            Modifier.weight(1f),
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        Text(
                            "${draft.eq[index]} dB",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Slider(
                        value = draft.eq[index].toFloat(),
                        onValueChange = { value ->
                            dirty = dirty + (index + 3)
                            sliderPending = true
                            draft =
                                draft.copy(
                                    eq =
                                        draft.eq.mapIndexed { i, gain ->
                                            if (i == index) value.roundToInt() else gain
                                        }
                                )
                        },
                        onValueChangeFinished = {
                            sliderPending = false
                            send(draft)
                        },
                        valueRange = -12f..12f,
                        steps = 23,
                        enabled = enabled,
                        modifier = Modifier.semantics { contentDescription = label },
                    )
                }
            }
        }
    }
}

@Composable
private fun VoiceToggle(
    title: String,
    checked: Boolean,
    enabled: Boolean,
    change: (Boolean) -> Unit,
) {
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 56.dp)
            .toggleable(
                value = checked,
                enabled = enabled,
                role = Role.Switch,
                onValueChange = change,
            )
            .semantics { contentDescription = title },
        horizontalArrangement = Arrangement.spacedBy(16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = null, enabled = enabled)
    }
}
