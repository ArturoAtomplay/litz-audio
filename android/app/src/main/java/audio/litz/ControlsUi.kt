package audio.litz

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp

@Composable
internal fun SettingValue(
    title: String,
    value: String,
    description: String? = null,
    enabled: Boolean = true,
) {
    Row(
        Modifier.fillMaxWidth().padding(20.dp),
        horizontalArrangement = Arrangement.spacedBy(16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(title, style = MaterialTheme.typography.titleMedium)
            Text(
                value,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            description?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        if (enabled) DisclosureIndicator()
    }
}

@Composable
internal fun DisclosureIndicator() {
    val color = MaterialTheme.colorScheme.onSurfaceVariant
    Canvas(Modifier.size(16.dp)) {
        drawLine(
            color,
            Offset(size.width * .35f, size.height * .2f),
            Offset(size.width * .65f, size.height * .5f),
            2.dp.toPx(),
            StrokeCap.Round,
        )
        drawLine(
            color,
            Offset(size.width * .65f, size.height * .5f),
            Offset(size.width * .35f, size.height * .8f),
            2.dp.toPx(),
            StrokeCap.Round,
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun AudioChoiceSheet(
    title: String,
    dismiss: () -> Unit,
    content: @Composable ColumnScope.() -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = dismiss,
        containerColor = MaterialTheme.colorScheme.surface,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Row(
            Modifier.fillMaxWidth().padding(start = 24.dp, end = 12.dp, bottom = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(title, Modifier.weight(1f), style = MaterialTheme.typography.titleLarge)
            TextButton(onClick = dismiss) { Text(stringResource(R.string.action_close)) }
        }
        Column(
            Modifier.fillMaxWidth()
                .weight(1f, fill = false)
                .verticalScroll(rememberScrollState())
                .selectableGroup()
                .padding(bottom = 24.dp),
            content = content,
        )
    }
}

@Composable
internal fun AudioChoiceRow(
    title: String,
    selected: Boolean,
    enabled: Boolean,
    description: String? = null,
    choose: () -> Unit,
) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = description?.let { { Text(it) } },
        leadingContent = { RadioButton(selected = selected, onClick = null, enabled = enabled) },
        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
        modifier =
            Modifier.selectable(
                selected = selected,
                enabled = enabled,
                role = Role.RadioButton,
                onClick = choose,
            ),
    )
}

@Composable
internal fun SectionHeader(title: String) {
    Text(
        title,
        Modifier.padding(start = 4.dp, top = 8.dp),
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
    )
}

@Composable
internal fun SettingRow(
    title: String,
    value: String? = null,
    enabled: Boolean = true,
    onClick: (() -> Unit)? = null,
    trailing: @Composable (() -> Unit)? = null,
) {
    Row(
        Modifier.fillMaxWidth()
            .then(
                if (onClick != null) Modifier.clickable(enabled = enabled, onClick = onClick)
                else Modifier
            )
            .padding(horizontal = 20.dp, vertical = 14.dp),
        horizontalArrangement = Arrangement.spacedBy(16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(
                title,
                style = MaterialTheme.typography.bodyLarge,
                color =
                    if (enabled) MaterialTheme.colorScheme.onSurface
                    else MaterialTheme.colorScheme.onSurface.copy(alpha = 0.38f),
            )
            value?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        when {
            trailing != null -> trailing()
            onClick != null && enabled -> DisclosureIndicator()
        }
    }
}

@Composable
internal fun quietSliderColors() =
    SliderDefaults.colors(
        activeTickColor = Color.Transparent,
        inactiveTickColor = Color.Transparent,
    )
