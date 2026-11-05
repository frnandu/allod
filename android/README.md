# Allod Android PoC

A minimal Nostr Wallet Connect wallet for Android, to try Allod's push notifications
(proposed NWC extension `register_push`) with UnifiedPush. Not for real funds.

It can:

- connect to a `nostr+walletconnect://` link (paste, scan a QR code, or open the link);
- show the balance;
- pay a BOLT11 invoice (paste or scan);
- create an invoice and show it as a QR code;
- turn on push notifications through a UnifiedPush distributor such as ntfy, and show a
  notification for every payment Allod pushes.

## Build

Needs JDK 17 and the Android SDK (`ANDROID_HOME`, or `sdk.dir` in `local.properties`).

```sh
cd android
./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

## Try push notifications

1. Run an Allod build that has `register_push` (Settings → Notifications lists devices
   "Via connection").
2. In Allod, create a connection with the **notifications** permission, plus receive and
   send for the wallet screens. Open its link on the phone, or scan its QR code in the app.
3. On the phone, install a UnifiedPush distributor: **ntfy** (Play Store or F-Droid),
   **Sunup** (Mozilla's push servers) or **gCompat-UP** (Google's, needs Play Services or
   microG). Without one, the app offers "Install ntfy" and the UnifiedPush list of
   distributors. Allod only accepts public `https` push endpoints, so a self-hosted ntfy
   needs a public https address.
4. Connect. When the wallet offers `register_push`, push turns on by itself with the
   default distributor (the one chosen before, else the first installed). The
   **Push distributor** dropdown switches to another one or turns push off.
   The **Activity log** shows each step; `register_push accepted: …` means push is on.
5. Pay an invoice from the app's Receive card, or any payment into this connection's
   account. The phone shows "Received … sats", even with the app closed.

Allod encrypts every push to the keys the app registered (RFC 8291); ntfy only relays
ciphertext. No Allod or app developer server is involved.

## How it works

- `Nwc.kt`: NIP-47 requests (kind 23194) and responses (kind 23195), NIP-44 encrypted,
  using the rust-nostr bindings for keys, encryption and relays.
- `App.kt`: the shared wallet connection, the UnifiedPush `PushReceiverService`
  (`onNewEndpoint` calls `register_push`, `onMessage` shows the notification), and the
  notification channel.
- `MainActivity.kt`: one Compose screen.

## Limits of this proof of concept

- The connection secret is stored in plain app preferences.
- Only BOLT11 invoices; no transaction list.
- No relay listener for notifications while the app is open; the balance refreshes when a
  push arrives or on request.
- One wallet connection at a time.
