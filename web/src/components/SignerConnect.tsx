import { Browser, DeviceMobile, Plugs, PuzzlePiece, Spinner } from "@phosphor-icons/react";
import { QRCodeSVG } from "qrcode.react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  bunkerSigner,
  chooseExtension,
  chooseLocalKey,
  deleteLocalKey,
  hasExtension,
  newSecretKey,
  MIN_PASSWORD_LENGTH,
  parseSecretKey,
  saveLocalKey,
  startNostrConnect,
  storedLocalNpub,
  unlockLocalKey,
  type Signer,
} from "../lib/signer";
import { copyText } from "../lib/clipboard";
import { shorten } from "../lib/utils";
import { Alert } from "./ui/alert";
import { Button } from "./ui/button";
import { Input, Label } from "./ui/input";

const card = "flex flex-col gap-3 rounded-[var(--radius-card)] border border-line bg-surface p-4 sm:flex-row sm:items-center";

const linkClass = "font-medium text-accent underline decoration-accent/40 underline-offset-2 hover:decoration-accent";

function ExternalLink({ href, children }: { href: string; children: ReactNode }) {
  return (
    <a href={href} target="_blank" rel="noreferrer" className={linkClass}>
      {children}
    </a>
  );
}

function SignerOption({
  icon,
  title,
  children,
  action,
  disabled,
  onClick,
}: {
  icon: ReactNode;
  title: string;
  children: ReactNode;
  action: string;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <div className={card}>
      {icon}
      <div className="min-w-0 flex-1 space-y-1">
        <p className="font-medium">{title}</p>
        <div className="text-sm text-muted">{children}</div>
      </div>
      <Button variant="secondary" disabled={disabled} onClick={onClick} className="w-full sm:w-auto sm:shrink-0">
        {action}
      </Button>
    </div>
  );
}

/** Lets the owner pick a Nostr signer: extension (NIP-07), remote signer (NIP-46) or a key kept in this browser. */
export function SignerConnect({ onConnected }: { onConnected: (signer: Signer, pubkey: string) => void }) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [bunker, setBunker] = useState("");
  const [pairing, setPairing] = useState<{ uri: string; cancel: () => void } | null>(null);
  const [paired, setPaired] = useState(false);
  const [authUrl, setAuthUrl] = useState<string | null>(null);
  const [showLocal, setShowLocal] = useState(false);
  const extension = hasExtension();
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      pairing?.cancel();
    };
  }, [pairing]);

  async function finish(signer: Signer) {
    const pubkey = await signer.getPublicKey();
    if (mounted.current) onConnected(signer, pubkey);
  }

  async function run(action: () => Promise<void>) {
    setError(null);
    setBusy(true);
    try {
      await action();
    } catch (e) {
      if (mounted.current) setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (mounted.current) setBusy(false);
    }
  }

  function pairWithApp() {
    pairing?.cancel();
    setShowLocal(false);
    setError(null);
    const p = startNostrConnect();
    setPairing(p);
    setPaired(false);
    // Not run(): waiting for the app must not lock the other options.
    p.signer
      .then((signer) => {
        if (mounted.current) setPaired(true);
        return finish(signer);
      })
      .catch((e) => {
      const msg = e instanceof Error ? e.message : String(e);
      if (mounted.current && msg !== "Pairing cancelled") setError(msg);
    });
  }

  return (
    <div className="space-y-5">
      <div className="space-y-3">
        <SignerOption
          icon={<PuzzlePiece size={28} weight="duotone" className="shrink-0 text-accent" />}
          title="Browser extension"
          action={extension ? "Use extension" : "Not detected"}
          disabled={!extension || busy}
          onClick={() => void run(async () => finish(chooseExtension()))}
        >
          {extension ? (
            <>
              <ExternalLink href="https://getalby.com/">Alby</ExternalLink>,{" "}
              <ExternalLink href="https://sidecar.top/">Sidecar</ExternalLink>,{" "}
              <ExternalLink href="https://github.com/fiatjaf/nos2x">nos2x</ExternalLink>,{" "}
              <ExternalLink href="https://keys.band/">Keys.band</ExternalLink> and other{" "}
              <ExternalLink href="https://github.com/nostr-protocol/nips/blob/master/07.md">NIP-07</ExternalLink>{" "}
              extensions
            </>
          ) : (
            <>
              No{" "}
              <ExternalLink href="https://github.com/nostr-protocol/nips/blob/master/07.md">NIP-07</ExternalLink>{" "}
              extension detected
            </>
          )}
        </SignerOption>
        <SignerOption
          icon={<DeviceMobile size={28} weight="duotone" className="shrink-0 text-accent" />}
          title="Signer app"
          action="Show QR code"
          disabled={busy}
          onClick={pairWithApp}
        >
          <ExternalLink href="https://github.com/greenart7c3/Amber">Amber</ExternalLink>,{" "}
          <ExternalLink href="https://github.com/nogringo/peridot">Peridot</ExternalLink> or any{" "}
          <ExternalLink href="https://github.com/nostr-protocol/nips/blob/master/46.md">NIP-46</ExternalLink> signer,
          via QR code
        </SignerOption>
        <SignerOption
          icon={<Browser size={28} weight="duotone" className="shrink-0 text-accent" />}
          title="Key in this browser"
          action="Create or import key"
          disabled={busy}
          onClick={() => {
            pairing?.cancel();
            setPairing(null);
            setShowLocal(true);
          }}
        >
          Create a new key or enter your nsec. It stays in this browser, encrypted with a password.
        </SignerOption>
      </div>

      {showLocal && <LocalKeyPanel busy={busy} onUse={() => void run(async () => finish(chooseLocalKey()))} />}

      {pairing && (
        <div className="flex flex-col items-center gap-3 rounded-[var(--radius-card)] border border-line bg-surface p-5 text-center">
          <QRCodeSVG value={pairing.uri} size={280} level="L" marginSize={2} className="h-auto w-full max-w-[280px] rounded-lg bg-white" />
          <p className="text-sm text-muted">Scan with your signer app, or copy the link into it.</p>
          <div className="flex gap-2">
            <Button size="sm" variant="secondary" onClick={() => void copyText(pairing.uri)}>
              Copy link
            </Button>
            <a href={pairing.uri} className="inline-flex h-8 items-center rounded-full px-3 text-sm text-accent">
              Open signer
            </a>
          </div>
          <p className="flex items-center gap-2 text-xs text-muted">
            <Spinner size={14} className="animate-spin" />{" "}
            {paired
              ? "Signer connected. Approve the request for your public key in the signer app…"
              : "Waiting for the signer to connect…"}
          </p>
        </div>
      )}

      <form
        className="space-y-2"
        onSubmit={(e) => {
          e.preventDefault();
          void run(async () => finish(await bunkerSigner(bunker, setAuthUrl)));
        }}
      >
        <Label htmlFor="bunker">Or paste a bunker:// link</Label>
        <div className="flex gap-2">
          <Input
            id="bunker"
            value={bunker}
            onChange={(e) => setBunker(e.target.value)}
            placeholder="bunker://… or name@domain"
            autoComplete="off"
            spellCheck={false}
          />
          <Button type="submit" variant="secondary" disabled={!bunker.trim() || busy}>
            <Plugs size={16} /> Connect
          </Button>
        </div>
      </form>

      {authUrl && (
        <Alert tone="warn" title="Your signer asks you to approve this connection">
          <a href={authUrl} target="_blank" rel="noreferrer" className="text-accent underline">
            Open the approval page
          </a>
        </Alert>
      )}
      {error && <Alert tone="danger" title="Could not connect to the signer">{error}</Alert>}
    </div>
  );
}

/** Lets the browser paint a spinner before scrypt (NIP-49) blocks for about a second. */
const nextFrame = () => new Promise((r) => setTimeout(r, 30));

/** Create, import, unlock or remove the password-encrypted key stored in this browser. */
function LocalKeyPanel({ busy, onUse }: { busy: boolean; onUse: () => void }) {
  const [stored, setStored] = useState(() => storedLocalNpub());
  const [fresh, setFresh] = useState<ReturnType<typeof newSecretKey> | null>(null);
  const [backedUp, setBackedUp] = useState(false);
  const [nsec, setNsec] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const importingEncrypted = nsec.trim().startsWith("ncryptsec1");

  async function act(f: () => void) {
    setError(null);
    setWorking(true);
    await nextFrame();
    try {
      f();
      setPassword("");
      setConfirm("");
      onUse();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setWorking(false);
    }
  }

  function store(sk: Uint8Array, again = confirm) {
    if (password !== again) throw new Error("The passwords do not match");
    saveLocalKey(sk, password);
    setStored(storedLocalNpub());
    setFresh(null);
    setNsec("");
  }

  const passwordFields = (
    <div className="grid gap-2 sm:grid-cols-2">
      <Input
        type="password"
        value={password}
        onChange={(e) => setPassword(e.target.value)}
        placeholder={`Password (${MIN_PASSWORD_LENGTH}+ characters)`}
        autoComplete="new-password"
        aria-label="Password"
      />
      <Input
        type="password"
        value={confirm}
        onChange={(e) => setConfirm(e.target.value)}
        placeholder="Password again"
        autoComplete="new-password"
        aria-label="Password again"
      />
    </div>
  );
  const disabled = busy || working;
  const spinner = working ? <Spinner size={16} className="animate-spin" /> : null;

  return (
    <div className="space-y-4 rounded-[var(--radius-card)] border border-line bg-surface p-5">
      <Alert tone="info" title="The key stays in this browser">
        It is stored in this browser's local storage, encrypted with a password you choose (
        <ExternalLink href="https://github.com/nostr-protocol/nips/blob/master/49.md">NIP-49</ExternalLink>), and never
        leaves it; Allod only sees signatures. You enter the password again after a reload, a reboot or signing out.
        Clearing the browser data deletes the key, so keep a copy of the nsec.
      </Alert>

      {stored && !fresh && (
        <form
          className="space-y-2"
          onSubmit={(e) => {
            e.preventDefault();
            void act(() => unlockLocalKey(password));
          }}
        >
          <Label htmlFor="unlock">
            Saved key <span className="font-mono text-xs font-normal">{shorten(stored, 14, 8)}</span>
          </Label>
          <div className="flex gap-2">
            <Input
              id="unlock"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              placeholder="Password"
              autoComplete="current-password"
            />
            <Button type="submit" disabled={disabled || !password}>
              {spinner} Unlock
            </Button>
          </div>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              if (!window.confirm("Delete the key from this browser? Without a copy of its nsec it is gone for good."))
                return;
              deleteLocalKey();
              setStored(null);
            }}
          >
            Remove from this browser
          </Button>
          <p className="text-xs text-muted">To use another key, remove this one first.</p>
        </form>
      )}

      {fresh ? (
        <form
          className="space-y-3"
          onSubmit={(e) => {
            e.preventDefault();
            void act(() => store(fresh.sk));
          }}
        >
          <Label>Your new secret key (nsec)</Label>
          <p className="break-all rounded-lg border border-line bg-bg p-3 font-mono text-xs select-all">{fresh.nsec}</p>
          <p className="text-xs text-muted">
            Public key: <span className="font-mono">{shorten(fresh.npub, 14, 8)}</span>
          </p>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant="secondary" onClick={() => void copyText(fresh.nsec)}>
              Copy nsec
            </Button>
            <Button size="sm" variant="ghost" onClick={() => setFresh(null)}>
              Cancel
            </Button>
          </div>
          <label className="flex items-start gap-2 text-sm">
            <input type="checkbox" checked={backedUp} onChange={(e) => setBackedUp(e.target.checked)} className="mt-1" />
            I wrote down or saved this nsec somewhere safe. Whoever has it controls this node.
          </label>
          {passwordFields}
          <Button type="submit" disabled={!backedUp || !password || disabled}>
            {spinner} Encrypt, save in this browser and continue
          </Button>
        </form>
      ) : stored ? null : (
        <div className="space-y-3">
          <Button
              variant="secondary"
              onClick={() => {
                setBackedUp(false);
                setPassword("");
                setConfirm("");
                setFresh(newSecretKey());
              }}
            >
            Create a new key
          </Button>
          <form
            className="space-y-2"
            onSubmit={(e) => {
              e.preventDefault();
              // An ncryptsec keeps its own password; anything else gets a new one.
              void act(() => store(parseSecretKey(nsec, password), importingEncrypted ? password : confirm));
            }}
          >
            <Label htmlFor="nsec">Or enter an existing nsec</Label>
            <Input
              id="nsec"
              type="password"
              value={nsec}
              onChange={(e) => setNsec(e.target.value)}
              placeholder="nsec1… or ncryptsec1…"
              autoComplete="off"
              spellCheck={false}
            />
            {nsec.trim() &&
              (importingEncrypted ? (
                <Input
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  placeholder="The ncryptsec's password"
                  aria-label="The ncryptsec's password"
                />
              ) : (
                passwordFields
              ))}
            <Button type="submit" variant="secondary" disabled={!nsec.trim() || !password || disabled}>
              {spinner} Encrypt and save
            </Button>
          </form>
        </div>
      )}
      {error && <Alert tone="danger">{error}</Alert>}
    </div>
  );
}
