package audio.litz

import android.content.Context
import androidx.annotation.StringRes
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.res.stringResource
import audio.litz.native.Failure

/** Text kept as a resource until it is shown, so it follows the current language. */
@Immutable
internal data class UiText(@StringRes val id: Int, val args: List<Any> = emptyList()) {
    @Composable fun resolve(): String = stringResource(id, *args.toTypedArray())

    fun resolve(context: Context): String = context.getString(id, *args.toTypedArray())
}

internal class UserFacingException(val text: UiText) : Exception()

internal fun Throwable.reason(): String = message ?: javaClass.simpleName

internal fun Throwable.text(): UiText =
    when (this) {
        is UserFacingException -> text
        is Failure -> failureText(this)
        else -> UiText(R.string.failure_internal, listOf(reason()))
    }
