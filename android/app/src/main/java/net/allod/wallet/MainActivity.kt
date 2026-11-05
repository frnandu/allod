package net.allod.wallet

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.graphics.Bitmap
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.MenuAnchorType
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import com.google.zxing.BarcodeFormat
import com.journeyapps.barcodescanner.BarcodeEncoder
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeout
import org.unifiedpush.android.connector.UnifiedPush

class MainActivity : ComponentActivity() {
    /** A nostr+walletconnect:// link the app was opened with. */
    private var openedLink by mutableStateOf<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        openedLink = intent?.dataString
        setContent {
            MaterialTheme {
                Surface(Modifier.fillMaxSize()) { WalletScreen(openedLink) }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        openedLink = intent.dataString
    }
}

@Composable
fun WalletScreen(openedLink: String?) {
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    var connected by remember { mutableStateOf(Wallet.savedUri(ctx) != null) }
    var status by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    val askPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {}
    // On start: renew the push registration (endpoints can change).
    LaunchedEffect(connected) {
        if (!connected) return@LaunchedEffect
        if (Wallet.hasPushKey(ctx)) {
            Wallet.refreshPush(ctx)
        } else if (!Wallet.pushOff(ctx)) {
            // Connected before push existed in this app: set it up now.
            runCatching { Wallet.client(ctx)?.getInfo() }.getOrNull()?.let { Wallet.setupPush(ctx, it) }
        }
    }

    // Runs an action with a status line and the busy flag.
    fun run(label: String, action: suspend () -> String) {
        busy = true
        status = "$label…"
        scope.launch {
            status = try {
                // Never leave the buttons disabled: give up after 90 s.
                withTimeout(90_000) { action() }
            } catch (e: TimeoutCancellationException) {
                "$label: no result after 90 s"
            } catch (e: Exception) {
                "$label failed: ${e.message}"
            }
            if (status.isNotEmpty()) Wallet.log(status)
            busy = false
        }
    }

    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Text("Allod PoC", style = MaterialTheme.typography.headlineMedium)
        if (status.isNotEmpty()) Text(status, style = MaterialTheme.typography.bodyMedium)
        if (!connected) {
            ConnectCard(openedLink, busy) { link ->
                if (Build.VERSION.SDK_INT >= 33) askPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
                run("Connecting") {
                    val info = Wallet.connect(ctx, link)
                    val push = Wallet.setupPush(ctx, info)
                    connected = true
                    "Connected; $push"
                }
            }
        } else {
            BalanceCard(::run)
            SendCard(busy, ::run)
            ReceiveCard(busy, ::run)
            PushCard(busy, ::run)
            LogCard()
            OutlinedButton(onClick = {
                run("Disconnecting") {
                    Wallet.disconnect(ctx)
                    connected = false
                    "Disconnected"
                }
            }) { Text("Disconnect") }
        }
    }
}

@Composable
private fun rememberScanner(onResult: (String) -> Unit) =
    rememberLauncherForActivityResult(ScanContract()) { result ->
        result.contents?.let(onResult)
    }

private fun scanOptions() = ScanOptions()
    .setDesiredBarcodeFormats(ScanOptions.QR_CODE)
    .setOrientationLocked(false)
    .setBeepEnabled(false)
    .setPrompt("")

private fun clipboardText(ctx: android.content.Context): String =
    ctx.getSystemService(ClipboardManager::class.java)
        .primaryClip?.getItemAt(0)?.coerceToText(ctx)?.toString().orEmpty()

@Composable
private fun ConnectCard(openedLink: String?, busy: Boolean, onConnect: (String) -> Unit) {
    val ctx = LocalContext.current
    var link by remember(openedLink) { mutableStateOf(openedLink.orEmpty()) }
    val scanner = rememberScanner { link = it }
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Connect a wallet", style = MaterialTheme.typography.titleMedium)
            Text("Paste or scan a nostr+walletconnect:// link from Allod (Connections → New connection, with notifications allowed).")
            OutlinedTextField(link, { link = it }, Modifier.fillMaxWidth(), label = { Text("NWC link") }, maxLines = 3)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = { link = clipboardText(ctx) }) { Text("Paste") }
                OutlinedButton(onClick = { scanner.launch(scanOptions()) }) { Text("Scan QR") }
                Button(enabled = !busy && link.isNotBlank(), onClick = { onConnect(link) }) { Text("Connect") }
            }
        }
    }
}

@Composable
private fun BalanceCard(run: (String, suspend () -> String) -> Unit) {
    val ctx = LocalContext.current
    var balance by remember { mutableStateOf<Long?>(null) }
    val refresh = {
        run("Loading balance") {
            balance = Wallet.client(ctx)?.getBalanceSats()
            ""
        }
    }
    LaunchedEffect(Unit) {
        refresh()
        // A payment notification arrived: show the new balance.
        Wallet.paymentEvents.collect { refresh() }
    }
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Balance", style = MaterialTheme.typography.titleMedium)
            Text(balance?.let { "$it sats" } ?: "…", style = MaterialTheme.typography.headlineSmall)
            OutlinedButton(onClick = refresh) { Text("Refresh") }
        }
    }
}

@Composable
private fun SendCard(busy: Boolean, run: (String, suspend () -> String) -> Unit) {
    val ctx = LocalContext.current
    var invoice by remember { mutableStateOf("") }
    val scanner = rememberScanner { invoice = it.removePrefix("lightning:").removePrefix("LIGHTNING:") }
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Send", style = MaterialTheme.typography.titleMedium)
            OutlinedTextField(invoice, { invoice = it }, Modifier.fillMaxWidth(), label = { Text("Lightning invoice") }, maxLines = 3)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = { invoice = clipboardText(ctx).removePrefix("lightning:") }) { Text("Paste") }
                OutlinedButton(onClick = { scanner.launch(scanOptions()) }) { Text("Scan QR") }
                Button(enabled = !busy && invoice.isNotBlank(), onClick = {
                    run("Paying") {
                        Wallet.client(ctx)?.payInvoice(invoice.trim())
                        invoice = ""
                        Wallet.paymentEvents.tryEmit(Unit)
                        "Paid"
                    }
                }) { Text("Pay") }
            }
        }
    }
}

@Composable
private fun ReceiveCard(busy: Boolean, run: (String, suspend () -> String) -> Unit) {
    val ctx = LocalContext.current
    var amount by remember { mutableStateOf("") }
    var invoice by remember { mutableStateOf<String?>(null) }
    var qr by remember { mutableStateOf<Bitmap?>(null) }
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Receive", style = MaterialTheme.typography.titleMedium)
            OutlinedTextField(amount, { amount = it.filter(Char::isDigit) }, label = { Text("Amount (sats)") })
            Button(enabled = !busy && amount.isNotBlank(), onClick = {
                run("Creating invoice") {
                    val inv = Wallet.client(ctx)?.makeInvoice(amount.toLong(), "Allod PoC") ?: ""
                    invoice = inv
                    qr = BarcodeEncoder().encodeBitmap(inv.uppercase(), BarcodeFormat.QR_CODE, 640, 640)
                    "Invoice ready"
                }
            }) { Text("Create invoice") }
            qr?.let { Image(it.asImageBitmap(), "Invoice QR code", Modifier.size(260.dp)) }
            invoice?.let { inv ->
                Text(inv, style = MaterialTheme.typography.bodySmall, maxLines = 3)
                OutlinedButton(onClick = {
                    ctx.getSystemService(ClipboardManager::class.java)
                        .setPrimaryClip(ClipData.newPlainText("invoice", inv))
                }) { Text("Copy") }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PushCard(busy: Boolean, run: (String, suspend () -> String) -> Unit) {
    val ctx = LocalContext.current
    val pushStatus by Wallet.pushStatus.collectAsState()
    var distributors by remember { mutableStateOf(Wallet.distributors(ctx)) }
    // null = off
    var selected by remember {
        mutableStateOf(if (Wallet.pushOff(ctx)) null else Wallet.preferredDistributor(ctx))
    }
    var expanded by remember { mutableStateOf(false) }

    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Push notifications", style = MaterialTheme.typography.titleMedium)
            Text("The distributor gives this app an endpoint, the app registers it with the wallet (register_push), and the wallet sends every payment to it, encrypted.")
            Text(
                pushStatus.ifEmpty {
                    Wallet.pushEndpoint(ctx)?.let { "Push on (registered earlier)" } ?: "Push off"
                },
                style = MaterialTheme.typography.bodyMedium,
            )
            if (distributors.isEmpty()) {
                Text("No UnifiedPush distributor is installed. Install one, then tap Check again.")
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = { openStore(ctx, "io.heckel.ntfy") }) { Text("Install ntfy") }
                    OutlinedButton(onClick = { openUrl(ctx, "https://unifiedpush.org/users/distributors/") }) {
                        Text("Other distributors")
                    }
                }
                OutlinedButton(onClick = {
                    distributors = Wallet.distributors(ctx)
                    if (distributors.isNotEmpty() && Wallet.hasPushKey(ctx)) {
                        val pkg = Wallet.preferredDistributor(ctx)!!
                        selected = pkg
                        Wallet.useDistributor(ctx, pkg)
                    }
                }) { Text("Check again") }
            } else {
                ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { if (!busy) expanded = it }) {
                    OutlinedTextField(
                        value = selected?.let { Wallet.distributorLabel(ctx, it) } ?: "Off",
                        onValueChange = {},
                        readOnly = true,
                        label = { Text("Push distributor") },
                        trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded) },
                        modifier = Modifier.menuAnchor(MenuAnchorType.PrimaryNotEditable).fillMaxWidth(),
                    )
                    ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                        DropdownMenuItem(text = { Text("Off") }, onClick = {
                            expanded = false
                            selected = null
                            run("Turning off push") {
                                Wallet.turnOffPush(ctx)
                                "Push off"
                            }
                        })
                        for (pkg in distributors) {
                            DropdownMenuItem(text = { Text(Wallet.distributorLabel(ctx, pkg)) }, onClick = {
                                expanded = false
                                selected = pkg
                                run("Switching push to ${Wallet.distributorLabel(ctx, pkg)}") {
                                    // Make sure the wallet's VAPID key is known (older connections).
                                    Wallet.client(ctx)?.getInfo()?.let { Wallet.setupPushKey(ctx, it) }
                                    Wallet.useDistributor(ctx, pkg)
                                    "Asked ${Wallet.distributorLabel(ctx, pkg)} for an endpoint"
                                }
                            })
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun LogCard() {
    val lines by Wallet.log.collectAsState()
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text("Activity log", style = MaterialTheme.typography.titleMedium)
            if (lines.isEmpty()) Text("Nothing yet.", style = MaterialTheme.typography.bodySmall)
            for (line in lines) Text(line, style = MaterialTheme.typography.bodySmall)
        }
    }
}

private fun openUrl(ctx: android.content.Context, url: String) {
    runCatching {
        ctx.startActivity(Intent(Intent.ACTION_VIEW, android.net.Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }
}

/** Opens an app's page in the installed store (Play, F-Droid, ...), else in the browser. */
private fun openStore(ctx: android.content.Context, pkg: String) {
    val market = Intent(Intent.ACTION_VIEW, android.net.Uri.parse("market://details?id=$pkg"))
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    runCatching { ctx.startActivity(market) }
        .onFailure { openUrl(ctx, "https://f-droid.org/packages/$pkg/") }
}
