package audio.litz

import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.StartOffset
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color

private val lowHeights = floatArrayOf(0.23f, 0.31f, 0.27f, 0.35f, 0.25f)
private val highHeights = floatArrayOf(0.54f, 0.85f, 0.69f, 1f, 0.62f)

@Composable
internal fun LitzBars(
    modifier: Modifier = Modifier,
    color: Color = MaterialTheme.colorScheme.primary,
) {
    val transition = rememberInfiniteTransition(label = "bars")
    val phases =
        List(5) { i ->
            transition.animateFloat(
                initialValue = 0f,
                targetValue = 1f,
                animationSpec =
                    infiniteRepeatable(
                        tween(420 + 60 * (i % 3), easing = FastOutSlowInEasing),
                        RepeatMode.Reverse,
                        StartOffset(70 * i),
                    ),
                label = "bar$i",
            )
        }
    Canvas(modifier) {
        val width = size.width / 9
        phases.forEachIndexed { i, phase ->
            val fraction = lowHeights[i] + (highHeights[i] - lowHeights[i]) * phase.value
            val height = maxOf(width, size.height * fraction)
            drawRoundRect(
                color,
                topLeft = Offset(width * 2 * i, (size.height - height) / 2),
                size = Size(width, height),
                cornerRadius = CornerRadius(width / 2),
            )
        }
    }
}
