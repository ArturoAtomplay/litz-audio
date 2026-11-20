package audio.litz

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import audio.litz.native.InputSensitivity
import audio.litz.native.Suppression
import audio.litz.native.VoiceSettings
import kotlin.math.log10
import kotlin.math.max
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

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun VoiceControls(
    computer: String?,
    revision: ULong,
    saved: VoicePreferences,
    enabled: Boolean,
    supported: List<Suppression> = Suppression.entries,
    failure: UiText? = null,
    inputLevel: (() -> Float?)? = null,
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
                    val level = inputLevel?.let { rememberLevel(it) }
                    val colors = quietSliderColors()
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
                        valueRange = THRESHOLD_LOW..THRESHOLD_HIGH,
                        steps = 39,
                        enabled = enabled,
                        colors = colors,
                        track = { state ->
                            if (level == null)
                                SliderDefaults.Track(state, colors = colors, enabled = enabled)
                            else LevelTrack(level, state.value, enabled)
                        },
                        modifier = Modifier.semantics { contentDescription = thresholdLabel },
                    )
                    Text(
                        stringResource(
                            if (level == null) R.string.voice_threshold_hint
                            else R.string.voice_threshold_level_hint
                        ),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
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

private const val THRESHOLD_LOW = -60f
private const val THRESHOLD_HIGH = -20f

/** The input level in dB, rising at once and falling 20 dB a second, updated every frame. */
@Composable
private fun rememberLevel(read: () -> Float?): State<Float> {
    val shown = remember { mutableFloatStateOf(THRESHOLD_LOW) }
    LaunchedEffect(read) {
        var last = 0L
        while (true) {
            withFrameNanos { now ->
                val db = read()?.let { 20f * log10(it.coerceAtLeast(0.000001f)) } ?: THRESHOLD_LOW
                val seconds = if (last == 0L) 0f else (now - last) / 1e9f
                last = now
                shown.floatValue =
                    max(db, shown.floatValue - 20f * seconds).coerceAtLeast(THRESHOLD_LOW)
            }
        }
    }
    return shown
}

/** The level on the threshold's scale, lit while it reaches the threshold. */
@Composable
private fun LevelTrack(level: State<Float>, threshold: Float, enabled: Boolean) {
    val colors = MaterialTheme.colorScheme
    val idle = colors.surfaceContainerHighest
    val open = colors.primary
    val closed = colors.onSurfaceVariant.copy(alpha = 0.38f)
    Canvas(
        Modifier.fillMaxWidth()
            .height(16.dp)
            .clip(RoundedCornerShape(8.dp))
            .alpha(if (enabled) 1f else 0.38f)
    ) {
        drawRect(idle)
        val db = level.value
        val fraction = ((db - THRESHOLD_LOW) / (THRESHOLD_HIGH - THRESHOLD_LOW)).coerceIn(0f, 1f)
        drawRect(
            if (db >= threshold) open else closed,
            size = size.copy(width = size.width * fraction),
        )
    }
}
