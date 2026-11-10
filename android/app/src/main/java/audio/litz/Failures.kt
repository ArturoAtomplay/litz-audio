package audio.litz

import audio.litz.native.Code
import audio.litz.native.Failure

/**
 * Words for the failures the native engine reports. `direction` is 0 for the microphone and 1 for
 * listening when the failure belongs to one of them.
 */
internal fun failureText(failure: Failure, direction: Int? = null): UiText =
    when (failure) {
        is Failure.Cancelled -> UiText(R.string.failure_cancelled)
        is Failure.ConnectTimeout -> UiText(R.string.failure_connect_timeout)
        is Failure.ControlTimeout -> UiText(R.string.failure_control_timeout)
        is Failure.AudioDiscontinuity -> UiText(R.string.failure_audio_discontinuity)
        is Failure.ConnectionLost -> UiText(R.string.failure_connection_lost)
        is Failure.Revoked -> UiText(R.string.failure_revoked)
        is Failure.Moved -> UiText(R.string.failure_moved)
        is Failure.Refused -> UiText(R.string.failure_refused)
        is Failure.ComputerRefused -> computerRefused(failure.reason, direction)
        is Failure.UnexpectedReply -> UiText(R.string.failure_unexpected_reply)
        is Failure.ProtocolMismatch -> UiText(R.string.failure_protocol_mismatch)
        is Failure.IdentityChanged -> UiText(R.string.failure_identity_changed)
        is Failure.InUse ->
            failure.owner
                ?.takeUnless { it.isBlank() }
                ?.let { UiText(R.string.failure_in_use, listOf(it)) }
                ?: UiText(R.string.failure_in_use_another_phone)
        is Failure.OutputsChanged -> UiText(R.string.failure_outputs_changed)
        is Failure.NotConnected -> UiText(R.string.failure_not_connected)
        is Failure.AudioUnavailable -> UiText(R.string.failure_audio_unavailable)
        is Failure.AudioDeviceDisconnected -> UiText(R.string.failure_audio_device_disconnected)
        is Failure.RouteDisconnected ->
            UiText(
                if (direction == 0) R.string.failure_input_disconnected
                else R.string.failure_output_disconnected
            )
        is Failure.PairingUnavailable -> UiText(R.string.failure_pairing_unavailable)
        is Failure.PairingDeclined -> UiText(R.string.failure_pairing_declined)
        is Failure.PairingFull -> UiText(R.string.failure_pairing_full)
        is Failure.PromptUnavailable -> UiText(R.string.failure_prompt_unavailable)
        is Failure.Internal -> UiText(R.string.failure_internal, listOf(failure.detail))
    }

private fun computerRefused(reason: Code, direction: Int?): UiText =
    when (reason) {
        Code.ROUTE_CONFLICT ->
            UiText(
                if (direction == 0) R.string.failure_no_virtual_microphone
                else R.string.failure_capture_unavailable
            )
        Code.BUSY -> UiText(R.string.failure_computer_busy)
        Code.UNAUTHORIZED -> UiText(R.string.failure_revoked)
        Code.REVISION_CONFLICT -> UiText(R.string.failure_outputs_changed)
        Code.UNSUPPORTED,
        Code.INVALID -> UiText(R.string.failure_computer_refused, listOf(reason.name))
    }
