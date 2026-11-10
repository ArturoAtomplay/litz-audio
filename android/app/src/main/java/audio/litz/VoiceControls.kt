package audio.litz

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import audio.litz.native.InputSensitivity
import audio.litz.native.Suppression
import audio.litz.native.VoiceSettings
import kotlin.math.log10
import kotlin.math.roundToInt
import kotlinx.coroutines.delay

@Immutable
internal data class VoicePreferences(
    val suppression: Suppression = Suppression.NONE,
    val level: Boolean = false,
    val eq: List<Int> = listOf(0, 0, 0),
    val sensitivity: String = "off",
    val threshold: Int = -40,
) {
    fun toSettings(): VoiceSettings =
        VoiceSettings(
            suppression = suppression,
            autoLevel = level,
            eqDb = eq.map { it.toByte() },
            inputSensitivity =
                when (sensitivity) {
                    "automatic" -> InputSensitivity.Automatic
                    "manual" -> InputSensitivity.Manual(threshold.toByte())
                    else -> InputSensitivity.Off
                },
        )

    companion object {
        fun from(settings: VoiceSettings) =
            VoicePreferences(
                settings.suppression,
                settings.autoLevel,
                settings.eqDb.map { it.toInt().coerceIn(-12, 12) },
                when (settings.inputSensitivity) {
                    is InputSensitivity.Automatic -> "automatic"
                    is InputSensitivity.Manual -> "manual"
                    is InputSensitivity.Off -> "off"
                },
                (settings.inputSensitivity as? InputSensitivity.Manual)
                    ?.v1
                    ?.toInt()
                    ?.coerceIn(-60, -20) ?: -40,
            )
    }
}

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
    revision: ULong,
    saved: VoicePreferences,
    enabled: Boolean,
    supported: List<Suppression> = Suppression.entries,
    failure: UiText? = null,
    inputRms: Float? = null,
    apply: (ULong, VoiceSettings) -> Unit,
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
        latestApply(latestRevision, next.toSettings())
    }
    LaunchedEffect(draft, sliderPending) {
        if (sliderPending && enabled) {
            delay(100)
            latestApply(latestRevision, draft.toSettings())
        }
    }
    val algorithms =
        listOf(
                Triple(Suppression.NONE, stringResource(R.string.noise_suppression_off), null),
                Triple(
                    Suppression.RNNOISE,
                    "RNNoise",
                    stringResource(R.string.noise_suppression_rnnoise_description),
                ),
                Triple(
                    Suppression.DEEP_FILTER_NET,
                    "DeepFilterNet",
                    stringResource(R.string.noise_suppression_deep_filter_description),
                ),
            )
            .filter { it.first in supported }
    val thresholdLabel = stringResource(R.string.voice_threshold)
    val bands =
        listOf(
            stringResource(R.string.equalizer_low),
            stringResource(R.string.equalizer_mid),
            stringResource(R.string.equalizer_high),
        )
    var suppressionSheet by remember { mutableStateOf(false) }
    var sensitivitySheet by remember { mutableStateOf(false) }
    OutlinedCard(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
        Column(Modifier.padding(vertical = 6.dp)) {
            SettingRow(
                stringResource(R.string.noise_suppression),
                algorithms.firstOrNull { it.first == draft.suppression }?.second
                    ?: stringResource(R.string.unavailable),
                enabled = enabled,
                onClick = { suppressionSheet = true },
            )
            SettingRow(
                stringResource(R.string.automatic_level),
                stringResource(R.string.automatic_level_description),
                enabled = enabled,
                onClick = { send(draft.copy(level = !draft.level)) },
            ) {
                Switch(checked = draft.level, onCheckedChange = null, enabled = enabled)
            }
            SettingRow(
                stringResource(R.string.input_sensitivity),
                when (draft.sensitivity) {
                    "automatic" -> stringResource(R.string.sensitivity_automatic)
                    "manual" -> stringResource(R.string.sensitivity_manual_value, draft.threshold)
                    else -> stringResource(R.string.sensitivity_off)
                },
                enabled = enabled,
                onClick = { sensitivitySheet = true },
            )
            if (draft.sensitivity == "manual")
                Column(Modifier.padding(horizontal = 20.dp)) {
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
                        colors = quietSliderColors(),
                        modifier = Modifier.semantics { contentDescription = thresholdLabel },
                    )
                    Text(
                        stringResource(R.string.voice_threshold_hint),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            inputRms?.let { rms ->
                val db = (20.0 * log10(rms.toDouble().coerceAtLeast(0.000001))).coerceIn(-60.0, 0.0)
                Column(
                    Modifier.padding(horizontal = 20.dp, vertical = 10.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Row {
                        Text(
                            stringResource(R.string.input_level),
                            Modifier.weight(1f),
                            style = MaterialTheme.typography.bodyLarge,
                        )
                        Text(
                            stringResource(R.string.decibels_full_scale, db.roundToInt()),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    LinearProgressIndicator(
                        progress = { ((db + 60.0) / 60.0).toFloat() },
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
            HorizontalDivider(Modifier.padding(horizontal = 20.dp, vertical = 6.dp))
            Text(
                stringResource(R.string.equalizer),
                Modifier.padding(horizontal = 20.dp, vertical = 8.dp),
                style = MaterialTheme.typography.bodyLarge,
            )
            bands.forEachIndexed { index, label ->
                Column(Modifier.padding(horizontal = 20.dp)) {
                    Row(Modifier.fillMaxWidth()) {
                        Text(
                            label,
                            Modifier.weight(1f),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Text(
                            stringResource(
                                R.string.decibels,
                                "${if (draft.eq[index] > 0) "+" else ""}${draft.eq[index]}",
                            ),
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
                        colors = quietSliderColors(),
                        modifier = Modifier.semantics { contentDescription = label },
                    )
                }
            }
        }
    }
    if (suppressionSheet)
        AudioChoiceSheet(
            stringResource(R.string.noise_suppression),
            dismiss = { suppressionSheet = false },
        ) {
            algorithms.forEach { (value, label, description) ->
                AudioChoiceRow(
                    label,
                    selected = draft.suppression == value,
                    enabled = enabled,
                    description = description,
                ) {
                    suppressionSheet = false
                    send(draft.copy(suppression = value))
                }
            }
        }
    if (sensitivitySheet)
        AudioChoiceSheet(
            stringResource(R.string.input_sensitivity),
            dismiss = { sensitivitySheet = false },
        ) {
            listOf(
                    Triple(
                        "off",
                        stringResource(R.string.sensitivity_off),
                        stringResource(R.string.sensitivity_off_description),
                    ),
                    Triple(
                        "automatic",
                        stringResource(R.string.sensitivity_automatic),
                        stringResource(R.string.sensitivity_automatic_description),
                    ),
                    Triple(
                        "manual",
                        stringResource(R.string.sensitivity_manual),
                        stringResource(R.string.sensitivity_manual_description),
                    ),
                )
                .forEach { (value, label, description) ->
                    AudioChoiceRow(
                        label,
                        selected = draft.sensitivity == value,
                        enabled = enabled,
                        description = description,
                    ) {
                        sensitivitySheet = false
                        send(draft.copy(sensitivity = value))
                    }
                }
        }
}
