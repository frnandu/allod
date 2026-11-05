package net.allod.wallet

import java.net.URLDecoder
import java.time.Duration
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeout
import org.json.JSONObject
import rust.nostr.sdk.Client
import rust.nostr.sdk.ClientBuilder
import rust.nostr.sdk.Event
import rust.nostr.sdk.EventBuilder
import rust.nostr.sdk.Filter
import rust.nostr.sdk.HandleNotification
import rust.nostr.sdk.Keys
import rust.nostr.sdk.Kind
import rust.nostr.sdk.NostrSigner
import rust.nostr.sdk.PublicKey
import rust.nostr.sdk.RelayMessage
import rust.nostr.sdk.RelayUrl
import rust.nostr.sdk.Tag
import rust.nostr.sdk.Timestamp

/** A parsed `nostr+walletconnect://` URI. */
data class NwcUri(val walletPubkey: String, val relays: List<String>, val secret: String) {
    companion object {
        fun parse(raw: String): NwcUri {
            val s = raw.trim()
            val rest = when {
                s.startsWith("nostr+walletconnect://") -> s.removePrefix("nostr+walletconnect://")
                s.startsWith("nostrwalletconnect://") -> s.removePrefix("nostrwalletconnect://")
                else -> throw IllegalArgumentException("not a nostr+walletconnect:// link")
            }
            val pubkey = rest.substringBefore('?').trimEnd('/')
            val params = rest.substringAfter('?', "").split('&').filter { it.contains('=') }.map {
                URLDecoder.decode(it.substringBefore('='), "UTF-8") to
                    URLDecoder.decode(it.substringAfter('='), "UTF-8")
            }
            val relays = params.filter { it.first == "relay" }.map { it.second }
            val secret = params.firstOrNull { it.first == "secret" }?.second
                ?: throw IllegalArgumentException("the link has no secret")
            require(pubkey.length == 64) { "the link has no wallet pubkey" }
            require(relays.isNotEmpty()) { "the link has no relay" }
            return NwcUri(pubkey, relays, secret)
        }
    }
}

/** An error the wallet service returned (NIP-47 `error`). */
class NwcException(val code: String, message: String) : Exception("$code: $message")

/**
 * Minimal NIP-47 client: NIP-44 encrypted requests (kind 23194) and responses
 * (kind 23195) over the link's relays.
 */
class NwcClient(val uri: NwcUri) {
    private val keys = Keys.parse(uri.secret)
    private val signer = NostrSigner.keys(keys)
    private val wallet = PublicKey.parse(uri.walletPubkey)
    private val client: Client = ClientBuilder().signer(signer).build()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val pending = ConcurrentHashMap<String, CompletableDeferred<JSONObject>>()
    private var listener: Job? = null

    suspend fun connect() {
        for (r in uri.relays) client.addRelay(RelayUrl.parse(r))
        // connect() only starts connecting: wait, so the first request is not dropped.
        client.connect()
        client.waitForConnection(Duration.ofSeconds(15))
        // Responses are ephemeral: subscribe before sending any request. Allow for a
        // phone clock that runs ahead of the wallet's.
        val since = Timestamp.fromSecs((Timestamp.now().asSecs() - 120u).coerceAtLeast(0u))
        client.subscribe(
            Filter()
                .kind(Kind(23195u))
                .author(wallet)
                .pubkey(keys.publicKey())
                .since(since),
        )
        listener = scope.launch {
            client.handleNotifications(object : HandleNotification {
                override suspend fun handle(relayUrl: RelayUrl, subscriptionId: String, event: Event) {
                    onResponse(event)
                }

                override suspend fun handleMsg(relayUrl: RelayUrl, msg: RelayMessage) {}
            })
        }
    }

    private suspend fun onResponse(event: Event) {
        val requestId = event.tags().eventIds().firstOrNull()?.toHex() ?: return
        val waiter = pending[requestId] ?: return
        runCatching { JSONObject(signer.nip44Decrypt(wallet, event.content())) }
            .onSuccess { waiter.complete(it) }
    }

    /** Sends one request and waits for its response; throws [NwcException] on an error. */
    suspend fun request(method: String, params: JSONObject = JSONObject()): JSONObject {
        val body = JSONObject().put("method", method).put("params", params).toString()
        val content = signer.nip44Encrypt(wallet, body)
        val event = EventBuilder(Kind(23194u), content)
            .tags(listOf(Tag.publicKey(wallet), Tag.parse(listOf("encryption", "nip44_v2"))))
            .signWithKeys(keys)
        val id = event.id().toHex()
        val waiter = CompletableDeferred<JSONObject>()
        pending[id] = waiter
        try {
            var sent = client.sendEvent(event)
            if (sent.success.isEmpty()) {
                // Android closes sockets of background apps: reconnect once and retry.
                client.connect()
                client.waitForConnection(Duration.ofSeconds(15))
                sent = client.sendEvent(event)
            }
            if (sent.success.isEmpty()) {
                val why = sent.failed.entries.joinToString { "${it.key}: ${it.value}" }
                throw NwcException("RELAY", "no relay accepted the request ($why)")
            }
            val response = try {
                withTimeout(60_000) { waiter.await() }
            } catch (e: kotlinx.coroutines.TimeoutCancellationException) {
                val relays = sent.success.joinToString()
                throw NwcException("TIMEOUT", "no answer from the wallet in 60 s (request sent to $relays)")
            }
            response.optJSONObject("error")?.let {
                throw NwcException(it.optString("code"), it.optString("message"))
            }
            return response.optJSONObject("result") ?: JSONObject()
        } finally {
            pending.remove(id)
        }
    }

    suspend fun getInfo() = request("get_info")

    /** Balance in sats. */
    suspend fun getBalanceSats(): Long = request("get_balance").optLong("balance") / 1000

    /** Pays a BOLT11 invoice; returns the preimage. */
    suspend fun payInvoice(invoice: String): String =
        request("pay_invoice", JSONObject().put("invoice", invoice)).optString("preimage")

    /** Creates a BOLT11 invoice; returns it. */
    suspend fun makeInvoice(sats: Long, description: String): String =
        request(
            "make_invoice",
            JSONObject().put("amount", sats * 1000).put("description", description),
        ).optString("invoice")

    /** Registers a Web Push / UnifiedPush endpoint (proposed NWC push extension). */
    suspend fun registerPush(endpoint: String, p256dh: String, auth: String, deviceName: String): JSONObject =
        request(
            "register_push",
            JSONObject()
                .put("endpoint", endpoint)
                .put("p256dh", p256dh)
                .put("auth", auth)
                .put("device_name", deviceName),
        )

    suspend fun unregisterPush(endpoint: String) {
        request("unregister_push", JSONObject().put("endpoint", endpoint))
    }

    suspend fun close() {
        listener?.cancel()
        client.shutdown()
    }
}
