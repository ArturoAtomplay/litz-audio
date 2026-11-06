package audio.litz

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import org.json.JSONObject

internal val microphoneSources =
    listOf(
        MicrophoneSource(0, "Automatic", "Recommended for most phones"),
        MicrophoneSource(1, "Standard microphone", "General recording mode"),
        MicrophoneSource(7, "Calls and meetings", "Optimized for voice calls"),
        MicrophoneSource(6, "Speech recognition", "Prioritizes clear speech"),
        MicrophoneSource(10, "Live performance", "Low latency when supported"),
        MicrophoneSource(5, "Camera microphone", "Camera recording mode"),
        MicrophoneSource(9, "Raw microphone", "Less processing when supported"),
    )

internal data class MicrophoneSource(val value: Int, val title: String, val description: String)

/** Manual pairing accepts an IPv4 address, with the normal Litz port by default. */
internal fun manualEndpoint(input: String): String? {
    val parts = input.trim().split(':')
    if (parts.size !in 1..2) return null
    val octets = parts[0].split('.')
    if (
        octets.size != 4 ||
            octets.any {
                it.isEmpty() ||
                    it.length > 3 ||
                    !it.all(Char::isDigit) ||
                    it.toIntOrNull()?.let { value -> value in 0..255 } != true
            }
    )
        return null
    val first = octets[0].toInt()
    if (first == 0 || first >= 224 || octets.all { it.toInt() == 0 }) return null
    val port = if (parts.size == 2) parts[1].toIntOrNull() else 47165
    if (port == null || port !in 1024..65535 || (parts.size == 2 && parts[1].any { !it.isDigit() }))
        return null
    return "${octets.joinToString(".") { it.toInt().toString() }}:$port"
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun DevicesApp(
    ui: UiState,
    permissionPending: Boolean,
    start: (Settings) -> Unit,
    stop: () -> Unit,
    connect: () -> Unit,
    disconnect: () -> Unit,
    pair: (NearbyComputer) -> Unit,
    cancelPair: () -> Unit,
    select: (SavedComputer) -> Unit,
    refresh: () -> Unit,
    edit: (Settings) -> Unit,
    apply: () -> Unit,
    forget: () -> Unit,
    route: (Long, Any) -> Unit = { _, _ -> },
    refreshRoutes: () -> Unit = {},
    voice: (Long, org.json.JSONObject) -> Unit = { _, _ -> },
) {
    var settings by rememberSaveable { mutableStateOf(false) }
    var candidate by remember { mutableStateOf<NearbyComputer?>(null) }
    var manualAddress by remember { mutableStateOf("") }
    var manualName by remember { mutableStateOf("") }
    var manualDialog by remember { mutableStateOf(false) }
    val homeScroll = rememberScrollState()
    val settingsScroll = rememberScrollState()
    BackHandler(enabled = settings) { settings = false }
    val status = remember(ui.session) { Snapshot(ui.session) }
    val busy = ui.session.audioPending || ui.pairing != null || permissionPending
    val selected = ui.saved.firstOrNull { it.fingerprint == ui.selected }
    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            Box(Modifier.fillMaxWidth().statusBarsPadding(), contentAlignment = Alignment.Center) {
                Row(
                    Modifier.widthIn(max = 600.dp)
                        .fillMaxWidth()
                        .padding(horizontal = 20.dp, vertical = 4.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        if (settings) "Audio settings" else "Litz",
                        modifier = Modifier.weight(1f),
                        style = MaterialTheme.typography.headlineSmall,
                    )
                    TextButton(onClick = { settings = !settings }) {
                        Text(if (settings) "Done" else "Settings")
                    }
                }
            }
        },
        bottomBar = { AdBanner() },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.TopCenter) {
            Column(
                Modifier.widthIn(max = 600.dp)
                    .fillMaxWidth()
                    .fillMaxHeight()
                    .verticalScroll(if (settings) settingsScroll else homeScroll)
                    .padding(horizontal = 20.dp, vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                ui.message?.takeUnless { it == ui.session.error }?.let { DeviceNotice(it) }
                ui.session.error?.let { DeviceNotice(it, error = true) }
                if (settings) {
                    var sources by remember { mutableStateOf(false) }
                    Text("Phone microphone", style = MaterialTheme.typography.titleLarge)
                    OutlinedCard(
                        onClick = { sources = true },
                        enabled = !busy,
                        modifier = Modifier.fillMaxWidth(),
                        shape = RoundedCornerShape(24.dp),
                    ) {
                        SettingValue(
                            "Recording mode",
                            microphoneSources.firstOrNull { it.value == ui.settings.source }?.title
                                ?: "Automatic",
                            recordingModeDescription(status),
                            enabled = !busy,
                        )
                    }
                    if (sources)
                        AudioChoiceSheet("Recording mode", dismiss = { sources = false }) {
                            microphoneSources.forEach { source ->
                                AudioChoiceRow(
                                    source.title,
                                    selected = ui.settings.source == source.value,
                                    enabled = !busy,
                                    description = source.description,
                                ) {
                                    edit(ui.settings.copy(source = source.value))
                                    apply()
                                    sources = false
                                }
                            }
                        }
                    voiceRuntimeDescription(status)?.let {
                        Text(it, style = MaterialTheme.typography.bodyMedium)
                    }
                    status.routes?.let { routes ->
                        routes.optJSONObject("voice")?.let { saved ->
                            VoiceControls(
                                ui.selected,
                                routes.getLong("revision"),
                                VoicePreferences.from(saved),
                                enabled = !busy && !status.inUse && status.connected,
                                supported =
                                    remember(
                                        routes.optJSONArray("suppression_algorithms")?.toString()
                                    ) {
                                        routes.optJSONArray("suppression_algorithms")?.let { values
                                            ->
                                            (0 until values.length()).map { values.getString(it) }
                                        } ?: emptyList()
                                    },
                                failure = ui.session.error,
                                inputRms = status.microphoneRms,
                                apply = voice,
                            )
                        }
                    }
                    selected?.let {
                        Text("Computer", style = MaterialTheme.typography.titleLarge)
                        OutlinedCard(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
                            Column(
                                Modifier.padding(20.dp),
                                verticalArrangement = Arrangement.spacedBy(8.dp),
                            ) {
                                Text(it.name, style = MaterialTheme.typography.titleMedium)
                                Text(
                                    status.connection,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                                OutlinedButton(
                                    onClick = if (status.connected) disconnect else connect,
                                    enabled = !busy,
                                ) {
                                    Text(if (status.connected) "Disconnect" else "Reconnect")
                                }
                            }
                        }
                        ComputerOutputControls(
                            status.routes,
                            enabled = !busy && status.connected && !status.inUse,
                            route = route,
                            refresh = refreshRoutes,
                        )
                        Text("Advanced", style = MaterialTheme.typography.titleLarge)
                        var details by rememberSaveable { mutableStateOf(false) }
                        ListItem(
                            colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                            headlineContent = { Text("Diagnostics") },
                            trailingContent = { Text(if (details) "−" else "+") },
                            modifier = Modifier.clickable { details = !details },
                        )
                        if (details)
                            Text(
                                diagnosticReport(ui, status),
                                style = MaterialTheme.typography.bodySmall,
                            )
                        var confirmForget by remember { mutableStateOf(false) }
                        ListItem(
                            colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                            headlineContent = {
                                Text("Forget computer", color = MaterialTheme.colorScheme.error)
                            },
                            modifier = Modifier.clickable(enabled = !busy) { confirmForget = true },
                        )
                        if (confirmForget)
                            AlertDialog(
                                containerColor = MaterialTheme.colorScheme.surface,
                                onDismissRequest = { confirmForget = false },
                                title = { Text("Forget ${it.name}?") },
                                text = {
                                    Text("Pair again by tapping the computer in the Nearby list.")
                                },
                                confirmButton = {
                                    TextButton(
                                        onClick = {
                                            confirmForget = false
                                            forget()
                                            settings = false
                                        }
                                    ) {
                                        Text("Forget", color = MaterialTheme.colorScheme.error)
                                    }
                                },
                                dismissButton = {
                                    TextButton(onClick = { confirmForget = false }) {
                                        Text("Cancel")
                                    }
                                },
                            )
                    }
                    StoreOptions()
                } else {
                    if (selected != null) {
                        Card(
                            Modifier.fillMaxWidth(),
                            shape = RoundedCornerShape(28.dp),
                            colors =
                                CardDefaults.cardColors(
                                    containerColor = MaterialTheme.colorScheme.secondaryContainer
                                ),
                        ) {
                            Column(
                                Modifier.padding(20.dp),
                                verticalArrangement = Arrangement.spacedBy(6.dp),
                            ) {
                                Text("My computer", style = MaterialTheme.typography.labelLarge)
                                Text(selected.name, style = MaterialTheme.typography.titleLarge)
                                Text(
                                    if (status.inUse) "In use by ${status.owner}"
                                    else if (status.state == "Streaming")
                                        "Audio active on this phone"
                                    else if (status.connected) "Connected · Ready for audio"
                                    else if (status.state == "Reconnecting") "Reconnecting…"
                                    else
                                        "Paired · ${if (ui.session.pending) "Connecting…" else "Offline"}"
                                )
                            }
                        }
                        if (
                            !status.connected &&
                                !status.reconnectAllowed &&
                                ui.session.error != null
                        ) {
                            ui.nearby
                                .firstOrNull { it.fingerprint == selected.fingerprint }
                                ?.let { computer ->
                                    TextButton(
                                        onClick = { candidate = computer },
                                        enabled = !busy,
                                    ) {
                                        Text("Pair again")
                                    }
                                }
                        }
                        if (status.inUse) {
                            FilledTonalButton(
                                onClick = {
                                    start(
                                        ui.settings.copy(
                                            mic = true,
                                            muted = false,
                                            takeover = status.generation,
                                        )
                                    )
                                },
                                enabled = !busy,
                                modifier = Modifier.fillMaxWidth().heightIn(min = 56.dp),
                            ) {
                                Text("Use this phone instead")
                            }
                            Text(
                                "Moves audio from ${status.owner} to this phone. Both phones stay remembered."
                            )
                        }
                        Text("Audio", style = MaterialTheme.typography.titleLarge)
                        DeviceDirection(
                            "Use phone microphone",
                            "Make your phone the microphone for your computer",
                            0,
                            ui.settings,
                            status.direction(0, ui.session),
                            status.error(0),
                            status.inUse,
                            busy,
                            start,
                        )
                        if (ui.settings.mic)
                            Text(
                                "Select Litz Microphone in your computer’s calling app.",
                                style = MaterialTheme.typography.bodyMedium,
                            )
                        DeviceDirection(
                            "Listen to computer",
                            "Hear your computer through your phone",
                            1,
                            ui.settings,
                            status.direction(1, ui.session),
                            status.error(1),
                            status.inUse,
                            busy,
                            start,
                        )
                        if (ui.settings.playback) {
                            OutlinedCard(
                                Modifier.fillMaxWidth(),
                                shape = RoundedCornerShape(24.dp),
                            ) {
                                Column(
                                    Modifier.padding(20.dp),
                                    verticalArrangement = Arrangement.spacedBy(4.dp),
                                ) {
                                    Row(
                                        Modifier.fillMaxWidth(),
                                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                                    ) {
                                        Text(
                                            "Listening volume",
                                            Modifier.weight(1f),
                                            style = MaterialTheme.typography.titleMedium,
                                        )
                                        Text(
                                            "${(ui.settings.playbackGain * 100).toInt()}%",
                                            style = MaterialTheme.typography.titleMedium,
                                        )
                                    }
                                    Slider(
                                        value = ui.settings.playbackGain,
                                        onValueChange = {
                                            edit(ui.settings.copy(playbackGain = it))
                                        },
                                        valueRange = 0f..2f,
                                        enabled = !busy && !status.inUse,
                                        modifier =
                                            Modifier.semantics {
                                                contentDescription = "Listening volume"
                                            },
                                        onValueChangeFinished = apply,
                                    )
                                    Text(
                                        "100% is the original volume. Raise it to boost quiet audio.",
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                            }
                        }
                    }
                    if (ui.pairing != null) {
                        Card(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
                            Column(
                                Modifier.padding(20.dp),
                                verticalArrangement = Arrangement.spacedBy(12.dp),
                            ) {
                                LitzBars(Modifier.size(width = 54.dp, height = 40.dp))
                                Text(
                                    "Connecting to ${ui.pairing}",
                                    style = MaterialTheme.typography.titleLarge,
                                )
                                val code = ui.pairingCode
                                if (code == null) {
                                    Text("Waiting for your computer…")
                                } else {
                                    Text("Allow the connection on your computer if it shows:")
                                    Text(code, style = MaterialTheme.typography.displaySmall)
                                    Text("If the codes differ, tap Cancel pairing.")
                                }
                                TextButton(onClick = cancelPair) { Text("Cancel pairing") }
                            }
                        }
                    }
                    if (ui.saved.size > 1) {
                        Text("My computers", style = MaterialTheme.typography.titleLarge)
                        ui.saved
                            .filter { it.fingerprint != ui.selected }
                            .forEach { computer ->
                                DeviceRow(computer.name, "Paired · Tap to use", !busy) {
                                    select(computer)
                                }
                            }
                    }
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            "Nearby computers",
                            style = MaterialTheme.typography.titleLarge,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(onClick = refresh, enabled = !busy) { Text("Refresh") }
                    }
                    val nearby =
                        ui.nearby.filter { n -> ui.saved.none { it.fingerprint == n.fingerprint } }
                    if (nearby.isEmpty())
                        Text(
                            if (selected == null)
                                "Start the Litz service on your computer and connect both devices to the same network. Your computer will appear here."
                            else if (ui.discovering) "Searching for more computers…"
                            else "No nearby computers found. Tap Refresh to try again.",
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    nearby.forEach { computer ->
                        DeviceRow(computer.name, "Nearby · Tap to pair", !busy) {
                            candidate = computer
                        }
                    }
                    OutlinedButton(
                        onClick = { manualDialog = true },
                        enabled = !busy,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text("Pair by IP address")
                    }
                    candidate?.let { computer ->
                        AlertDialog(
                            containerColor = MaterialTheme.colorScheme.surface,
                            onDismissRequest = { candidate = null },
                            title = { Text("Pair with ${computer.name}?") },
                            text = {
                                Column(
                                    Modifier.verticalScroll(rememberScrollState()),
                                    verticalArrangement = Arrangement.spacedBy(12.dp),
                                ) {
                                    Text(
                                        "Tap Pair, then allow the connection in the prompt on ${computer.name}. This is only needed once."
                                    )
                                }
                            },
                            confirmButton = {
                                TextButton(
                                    onClick = {
                                        pair(computer)
                                        candidate = null
                                    },
                                    enabled = !busy,
                                ) {
                                    Text("Pair")
                                }
                            },
                            dismissButton = {
                                TextButton(onClick = { candidate = null }) { Text("Cancel") }
                            },
                        )
                    }
                }
                if (manualDialog) {
                    val endpoint = manualEndpoint(manualAddress)
                    AlertDialog(
                        onDismissRequest = { manualDialog = false },
                        title = { Text("Pair by IP address") },
                        text = {
                            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                                Text(
                                    "Enter the computer's local IPv4 address. Litz verifies the computer's identity before saving it."
                                )
                                OutlinedTextField(
                                    value = manualAddress,
                                    onValueChange = { manualAddress = it.take(21) },
                                    label = { Text("IP address") },
                                    placeholder = { Text("192.168.1.10") },
                                    singleLine = true,
                                    modifier = Modifier.fillMaxWidth(),
                                )
                                OutlinedTextField(
                                    value = manualName,
                                    onValueChange = { manualName = it.take(64) },
                                    label = { Text("Computer name (optional)") },
                                    singleLine = true,
                                )
                            }
                        },
                        confirmButton = {
                            TextButton(
                                onClick = {
                                    pair(
                                        NearbyComputer(
                                            manualName.trim().ifEmpty { "Computer" },
                                            "",
                                            endpoint!!,
                                        )
                                    )
                                    manualDialog = false
                                },
                                enabled = endpoint != null,
                            ) {
                                Text("Pair")
                            }
                        },
                        dismissButton = {
                            TextButton(onClick = { manualDialog = false }) { Text("Cancel") }
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun ComputerOutputControls(
    routes: JSONObject?,
    enabled: Boolean,
    route: (Long, Any) -> Unit,
    refresh: () -> Unit,
) {
    var outputs by remember { mutableStateOf(false) }
    OutlinedCard(
        onClick = { outputs = true },
        enabled = enabled && routes != null,
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(24.dp),
    ) {
        SettingValue(
            "Listening from",
            routes?.let { routeDescription(it.opt("effective"), it) } ?: "Unavailable",
            if (enabled) "Choose which computer output to hear"
            else "Connect to an available computer to change its output",
            enabled = enabled && routes != null,
        )
    }
    TextButton(onClick = refresh, enabled = enabled) { Text("Refresh outputs") }
    if (outputs && routes != null) {
        val mirror = routes.optJSONObject("effective")?.optJSONObject("Mirror")
        fun choose(name: String?) {
            route(
                routes.getLong("revision"),
                JSONObject().put("Mirror", JSONObject().put("name", name ?: JSONObject.NULL)),
            )
            outputs = false
        }
        AudioChoiceSheet("Computer audio output", dismiss = { outputs = false }) {
            AudioChoiceRow(
                "Follow default output",
                selected = mirror?.isNull("name") == true,
                enabled = enabled,
                description = "Switches when your computer changes output",
            ) {
                choose(null)
            }
            routes.optJSONArray("endpoints")?.let { endpoints ->
                for (i in 0 until endpoints.length()) {
                    val device = endpoints.getJSONObject(i)
                    val name = device.getString("name")
                    AudioChoiceRow(
                        device.getString("label"),
                        selected = mirror?.optString("name") == name,
                        enabled = enabled,
                    ) {
                        choose(name)
                    }
                }
            }
        }
    }
}

@Composable
private fun DeviceRow(name: String, state: String, enabled: Boolean, action: () -> Unit) {
    OutlinedCard(
        onClick = action,
        enabled = enabled,
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(24.dp),
    ) {
        SettingValue(name, state, enabled = enabled)
    }
}

@Composable
private fun DeviceDirection(
    title: String,
    description: String,
    index: Int,
    settings: Settings,
    direction: String,
    error: String?,
    inUse: Boolean,
    busy: Boolean,
    start: (Settings) -> Unit,
) {
    val enabled = if (index == 0) settings.mic else settings.playback
    OutlinedCard(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(
                Modifier.fillMaxWidth()
                    .toggleable(
                        value = enabled,
                        enabled = !busy && !inUse,
                        role = Role.Switch,
                        onValueChange = { value ->
                            start(
                                if (index == 0)
                                    settings.copy(mic = value, muted = false, input = 0, output = 0)
                                else settings.copy(playback = value, input = 0, output = 0)
                            )
                        },
                    )
                    .semantics { contentDescription = title },
                horizontalArrangement = Arrangement.spacedBy(16.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(title, Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                Switch(checked = enabled, onCheckedChange = null, enabled = !busy && !inUse)
            }
            Text(
                description,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                direction,
                style = MaterialTheme.typography.labelLarge,
                color =
                    when (direction) {
                        "Running" -> MaterialTheme.colorScheme.primary
                        "Error" -> MaterialTheme.colorScheme.error
                        else -> MaterialTheme.colorScheme.onSurfaceVariant
                    },
            )
            if (index == 0 && enabled)
                OutlinedButton(
                    onClick = { start(settings.copy(muted = !settings.muted)) },
                    enabled = !busy && !inUse,
                ) {
                    Text(if (settings.muted) "Unmute microphone" else "Mute microphone")
                }
            error?.let { DeviceNotice(it, error = true) }
        }
    }
}

@Composable
private fun DeviceNotice(text: String, error: Boolean = false) {
    Surface(
        modifier = Modifier.fillMaxWidth(),
        color =
            if (error) MaterialTheme.colorScheme.errorContainer
            else MaterialTheme.colorScheme.surfaceVariant,
        contentColor =
            if (error) MaterialTheme.colorScheme.onErrorContainer
            else MaterialTheme.colorScheme.onSurfaceVariant,
        shape = RoundedCornerShape(20.dp),
    ) {
        Text(text, Modifier.padding(16.dp))
    }
}
