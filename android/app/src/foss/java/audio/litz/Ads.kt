package audio.litz

import androidx.activity.ComponentActivity
import androidx.compose.foundation.clickable
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalUriHandler

internal object Ads {
    fun start(activity: ComponentActivity) = Unit
}

@Composable internal fun AdBanner() = Unit

@Composable
internal fun StoreOptions() {
    val uriHandler = LocalUriHandler.current
    ListItem(
        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
        headlineContent = { Text("Support Litz") },
        supportingContent = { Text("Litz is free and has no ads. A donation keeps it that way.") },
        modifier = Modifier.clickable { uriHandler.openUri("https://ko-fi.com/arturoatomplay") },
    )
}
