package audio.litz

import android.content.Context
import androidx.activity.ComponentActivity
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.content.edit

private const val SUPPORT_URL = "https://ko-fi.com/arturoatomplay"
private const val SNOOZE_MS = 30L * 24 * 60 * 60 * 1000

internal object Ads {
    fun start(activity: ComponentActivity) = Unit
}

@Composable
internal fun AdBanner() {
    val context = LocalContext.current
    val prefs = remember { context.getSharedPreferences("support", Context.MODE_PRIVATE) }
    var visible by remember {
        mutableStateOf(
            System.currentTimeMillis() - prefs.getLong("snoozed_at", 0L) !in 0..SNOOZE_MS
        )
    }
    val uriHandler = LocalUriHandler.current
    AnimatedVisibility(visible) {
        Column(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surface)) {
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            Row(
                Modifier.fillMaxWidth()
                    .navigationBarsPadding()
                    .heightIn(min = 64.dp)
                    .padding(start = 16.dp, end = 4.dp, top = 8.dp, bottom = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Box(
                    Modifier.size(32.dp)
                        .background(MaterialTheme.colorScheme.primaryContainer, CircleShape),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        painterResource(R.drawable.ic_support),
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.onPrimaryContainer,
                        modifier = Modifier.size(18.dp),
                    )
                }
                Column(Modifier.weight(1f)) {
                    Text(
                        stringResource(R.string.support_title),
                        style = MaterialTheme.typography.titleSmall,
                        color = MaterialTheme.colorScheme.onSurface,
                    )
                    Text(
                        stringResource(R.string.support_body),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    FilledTonalButton(
                        onClick = { uriHandler.openUri(SUPPORT_URL) },
                        contentPadding = PaddingValues(horizontal = 16.dp),
                    ) {
                        Text(stringResource(R.string.support_action))
                    }
                    IconButton(
                        onClick = {
                            prefs.edit { putLong("snoozed_at", System.currentTimeMillis()) }
                            visible = false
                        }
                    ) {
                        Icon(
                            painterResource(R.drawable.ic_close),
                            contentDescription = stringResource(R.string.support_later),
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.size(20.dp),
                        )
                    }
                }
            }
        }
    }
}

@Composable
internal fun StoreOptions() {
    val uriHandler = LocalUriHandler.current
    SettingRow(
        stringResource(R.string.support_setting),
        stringResource(R.string.support_setting_description),
        onClick = { uriHandler.openUri(SUPPORT_URL) },
    )
}
