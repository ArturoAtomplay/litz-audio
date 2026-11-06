package audio.litz

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

@Composable
internal fun LitzTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    MaterialTheme(
        colorScheme =
            if (darkTheme)
                darkColorScheme(
                    primary = Color(0xFFB7D9C5),
                    onPrimary = Color(0xFF193829),
                    background = Color(0xFF101715),
                    onBackground = Color(0xFFE3E8E4),
                    surface = Color(0xFF18211E),
                    onSurface = Color(0xFFE3E8E4),
                    surfaceVariant = Color(0xFF3E4842),
                    onSurfaceVariant = Color(0xFFBCC6BF),
                    outline = Color(0xFF87928B),
                    outlineVariant = Color(0xFF434C47),
                    surfaceContainerHighest = Color(0xFF29322D),
                    secondaryContainer = Color(0xFF293E34),
                    onSecondaryContainer = Color(0xFFDCEAE0),
                )
            else
                lightColorScheme(
                    primary = Color(0xFF36674F),
                    background = Color(0xFFF5F8F4),
                    onBackground = Color(0xFF1B211D),
                    surface = Color(0xFFFBFCFA),
                    onSurface = Color(0xFF1B211D),
                    surfaceVariant = Color(0xFFE2E7E2),
                    onSurfaceVariant = Color(0xFF505A54),
                    outline = Color(0xFF727D75),
                    outlineVariant = Color(0xFFC4CDC6),
                    surfaceContainerHighest = Color(0xFFE4EAE5),
                    secondaryContainer = Color(0xFFD9EBDD),
                    onSecondaryContainer = Color(0xFF1C3527),
                ),
        content = content,
    )
}
