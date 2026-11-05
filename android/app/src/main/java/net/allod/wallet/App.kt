package net.allod.wallet

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.util.Log
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.json.JSONObject
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.UnifiedPush
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage

class App : Application() {
    override fun onCreate() {
        super.onCreate()
        Notifier.createChannel(this)
    }
}

/** The one wallet connection of this proof of concept, shared by the UI and the push service. */
object Wallet {
    private const val PREFS = "wallet"
    private val lock = Mutex()
    private var client: NwcClient? = null

    val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** Human-readable push state, shown in the UI. */
    val pushStatus = MutableStateFlow("")

    /** Recent steps and errors, newest first, shown in the app and written to logcat. */
    val log = MutableStateFlow<List<String>>(emptyList())

    fun log(msg: String) {
        Log.i("AllodPoC", msg)
        val time = SimpleDateFormat("HH:mm:ss", Locale.US).format(Date())
        log.value = (listOf("$time  $msg") + log.value).take(60)
    }

    /** Emits when a payment notification arrives, so the UI refreshes the balance. */
    val paymentEvents = MutableSharedFlow<Unit>(extraBufferCapacity = 8)

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun savedUri(ctx: Context): String? = prefs(ctx).getString("uri", null)

    fun pushEndpoint(ctx: Context): String? = prefs(ctx).getString("push_endpoint", null)

    fun setPushEndpoint(ctx: Context, endpoint: String?) =
        prefs(ctx).edit().putString("push_endpoint", endpoint).apply()

    /** The connected client, connecting from the saved link when needed. */
    suspend fun client(ctx: Context): NwcClient? = lock.withLock {
        client ?: savedUri(ctx)?.let { raw ->
            NwcClient(NwcUri.parse(raw)).also {
                it.connect()
                client = it
            }
        }
    }

    /** Connects to a new link, checks it with `get_info`, and saves it. */
    suspend fun connect(ctx: Context, raw: String): JSONObject {
        val fresh = NwcClient(NwcUri.parse(raw))
        fresh.connect()
        val info = try {
            fresh.getInfo()
        } catch (e: Exception) {
            fresh.close()
            throw e
        }
        lock.withLock {
            client?.close()
            client = fresh
        }
        prefs(ctx).edit().putString("uri", raw.trim()).remove("push_endpoint").apply()
        return info
    }

    suspend fun disconnect(ctx: Context) {
        runCatching { turnOffPush(ctx) }
        lock.withLock {
            client?.close()
            client = null
        }
        prefs(ctx).edit().clear().apply()
    }

    // ------------------------------------------------------------------ push

    /** Installed UnifiedPush distributors (package names): ntfy, Sunup, gCompat-UP, ... */
    fun distributors(ctx: Context): List<String> = UnifiedPush.getDistributors(ctx)

    fun distributorLabel(ctx: Context, pkg: String): String = runCatching {
        val pm = ctx.packageManager
        pm.getApplicationLabel(pm.getApplicationInfo(pkg, 0)).toString()
    }.getOrDefault(pkg)

    /** The user turned push off: do not turn it on again by itself. */
    fun pushOff(ctx: Context) = prefs(ctx).getBoolean("push_off", false)

    /** The saved distributor if still installed, else the first installed one. */
    /**
     * App settings that outlive a wallet connection (the wallet prefs are cleared on
     * disconnect).
     */
    private fun settings(ctx: Context) = ctx.getSharedPreferences("settings", Context.MODE_PRIVATE)

    /**
     * The distributor for push: the one picked in the dropdown (kept across connections),
     * else the one the UnifiedPush library saved, else the first installed one.
     */
    fun preferredDistributor(ctx: Context): String? {
        val all = distributors(ctx)
        return settings(ctx).getString("default_distributor", null)?.takeIf { it in all }
            ?: UnifiedPush.getSavedDistributor(ctx)?.takeIf { it in all }
            ?: all.firstOrNull()
    }

    /**
     * After connecting: turns push on with the preferred distributor when the wallet
     * offers `register_push`. Returns what happened, for the status line.
     */
    fun setupPush(ctx: Context, info: JSONObject): String {
        val methods = info.optJSONArray("methods")?.toString().orEmpty()
        val vapid = info.optString("push_vapid_key")
        if (!methods.contains("\"register_push\"") || vapid.isEmpty()) {
            log("Wallet does not offer register_push; push stays off")
            prefs(ctx).edit().remove("vapid").apply()
            return "push not offered by this wallet"
        }
        prefs(ctx).edit().putString("vapid", vapid).apply()
        if (pushOff(ctx)) return "push off (your choice)"
        val pkg = preferredDistributor(ctx) ?: run {
            log("No UnifiedPush distributor installed")
            return "install a UnifiedPush distributor for push"
        }
        useDistributor(ctx, pkg)
        return "turning on push with ${distributorLabel(ctx, pkg)}"
    }

    /** Stores the wallet's VAPID key from `get_info`, if it offers `register_push`. */
    fun setupPushKey(ctx: Context, info: JSONObject) {
        val vapid = info.optString("push_vapid_key")
        if (vapid.isNotEmpty()) prefs(ctx).edit().putString("vapid", vapid).apply()
    }

    fun hasPushKey(ctx: Context) = prefs(ctx).getString("vapid", null) != null

    /** Asks the saved distributor again (on app start): picks up a rotated endpoint. */
    fun refreshPush(ctx: Context) {
        val vapid = prefs(ctx).getString("vapid", null) ?: return
        if (pushOff(ctx)) return
        val pkg = preferredDistributor(ctx) ?: return
        UnifiedPush.saveDistributor(ctx, pkg)
        UnifiedPush.register(ctx, vapid = vapid)
    }

    /**
     * Uses `pkg` for push: unregisters the old endpoint (wallet and distributor) when
     * switching, then asks `pkg` for an endpoint. [PushReceiverService.onNewEndpoint]
     * registers it with the wallet.
     */
    fun useDistributor(ctx: Context, pkg: String) {
        val vapid = prefs(ctx).getString("vapid", null)
        if (vapid == null) {
            log("This wallet does not offer register_push")
            return
        }
        val previous = UnifiedPush.getSavedDistributor(ctx)
        if (previous != null && previous != pkg) {
            pushEndpoint(ctx)?.let { old -> scope.launch { runCatching { client(ctx)?.unregisterPush(old) } } }
            setPushEndpoint(ctx, null)
            UnifiedPush.unregister(ctx)
            log("Left ${distributorLabel(ctx, previous)}")
        }
        prefs(ctx).edit().putBoolean("push_off", false).apply()
        // The default for this and every later connection.
        settings(ctx).edit().putString("default_distributor", pkg).apply()
        UnifiedPush.saveDistributor(ctx, pkg)
        UnifiedPush.register(ctx, vapid = vapid)
        pushStatus.value = "Waiting for ${distributorLabel(ctx, pkg)}…"
        log("Asked ${distributorLabel(ctx, pkg)} for an endpoint")
    }

    /** Turns push off: the wallet forgets the endpoint, the distributor the app. */
    suspend fun turnOffPush(ctx: Context) {
        pushEndpoint(ctx)?.let { client(ctx)?.unregisterPush(it) }
        setPushEndpoint(ctx, null)
        UnifiedPush.unregister(ctx)
        prefs(ctx).edit().putBoolean("push_off", true).apply()
        pushStatus.value = "Push off"
    }
}

/** Receives UnifiedPush events from the distributor app (for example ntfy). */
class PushReceiverService : PushService() {
    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        Wallet.log("Distributor gave an endpoint on ${hostOf(endpoint.url)} (temporary=${endpoint.temporary})")
        val keys = endpoint.pubKeySet
        if (keys == null) {
            Wallet.pushStatus.value = "The distributor gave no encryption keys"
            Wallet.log("No encryption keys from the distributor")
            return
        }
        val ctx = applicationContext
        Wallet.scope.launch {
            try {
                val client = Wallet.client(ctx) ?: return@launch
                Wallet.log("Sending register_push to the wallet")
                // Shown in Allod on the connection's page, e.g. "Pixel 9 · Sunup".
                val distributor = UnifiedPush.getAckDistributor(ctx)?.let { Wallet.distributorLabel(ctx, it) }
                val name = listOfNotNull(android.os.Build.MODEL, distributor).joinToString(" · ")
                val result = client.registerPush(endpoint.url, keys.pubKey, keys.auth, name)
                Wallet.setPushEndpoint(ctx, endpoint.url)
                val types = result.optJSONArray("notification_types")?.join(", ")?.replace("\"", "") ?: ""
                Wallet.pushStatus.value = "Push on via ${hostOf(endpoint.url)} ($types)"
                Wallet.log("register_push accepted: $types")
            } catch (e: Exception) {
                Wallet.pushStatus.value = "register_push failed: ${e.message}"
                Wallet.log("register_push failed: ${e.message}")
            }
        }
    }

    override fun onMessage(message: PushMessage, instance: String) {
        Wallet.log("Push received (${message.content.size} bytes, decrypted=${message.decrypted})")
        if (!message.decrypted) return
        runCatching { JSONObject(String(message.content)) }.onSuccess {
            Notifier.showPayment(applicationContext, it)
            Wallet.paymentEvents.tryEmit(Unit)
        }
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {
        Wallet.pushStatus.value = "UnifiedPush registration failed: $reason"
        Wallet.log("Distributor refused the registration: $reason")
    }

    override fun onUnregistered(instance: String) {
        Wallet.pushStatus.value = "Push off"
        Wallet.log("Distributor unregistered this app")
    }

    private fun hostOf(url: String) = url.substringAfter("://").substringBefore('/')
}

object Notifier {
    private const val CHANNEL = "payments"

    fun createChannel(ctx: Context) {
        val channel = NotificationChannel(CHANNEL, "Payments", NotificationManager.IMPORTANCE_HIGH)
        ctx.getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    /** Shows a NWC push payload: `{event_id, notification_type, notification}`. */
    fun showPayment(ctx: Context, payload: JSONObject) {
        val n = payload.optJSONObject("notification") ?: JSONObject()
        val sats = n.optLong("amount") / 1000
        val title = when (payload.optString("notification_type").ifEmpty { payload.optString("type") }) {
            "test" -> "Allod test notification"
            "payment_received" -> "Received $sats sats"
            "payment_sent" -> "Sent $sats sats"
            "hold_invoice_accepted" -> "Hold invoice accepted: $sats sats"
            else -> "Wallet activity"
        }
        val body = n.optString("description").ifEmpty { "Tap to open the wallet" }
        val open = PendingIntent.getActivity(
            ctx,
            0,
            Intent(ctx, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(ctx, CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(title)
            .setContentText(body)
            .setContentIntent(open)
            .setAutoCancel(true)
            .build()
        // The event id identifies the payment, so a repeated push replaces its notification.
        val id = payload.optString("event_id").hashCode()
        val manager = NotificationManagerCompat.from(ctx)
        if (!manager.areNotificationsEnabled()) {
            Wallet.log("Notification not shown: notifications are disabled for this app")
            return
        }
        runCatching { manager.notify(id, notification) }
            .onSuccess { Wallet.log("Notification shown: $title") }
            .onFailure { Wallet.log("Notification not shown: ${it.message}") }
    }
}
