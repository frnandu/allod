import { useState } from "react";
import useSWR, { type KeyedMutator } from "swr";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../../components/ui/card";
import { Input, Label } from "../../components/ui/input";
import {
  fetcher,
  saveChainSource,
  startStandbyBitcoind,
  type ActiveChainSource,
  type ChainSourceRequest,
  type ChainSourceSettings,
  type ChainSourceView,
} from "../../lib/api";
import { cn } from "../../lib/utils";
import { SettingsGroup } from "./SettingsLayout";

const KEY = "/api/settings/chain-source";

const KIND_NAME = { bitcoind: "Bitcoin Core", electrum: "Electrum", esplora: "Esplora" } as const;

/** What ALLOD_CHAIN means, for the "stack default" choice. */
const MODE_NAME: Record<string, string> = {
  bundled: "Bitcoin Core in this stack",
  "bundled-rest": "Bitcoin Core in this stack (REST)",
  "external-bitcoind": "Bitcoin Core node from the stack settings",
  electrum: "Electrum server from the stack settings",
  esplora: "Esplora server from the stack settings",
};

function host(url: string): string {
  return url.replace(/^[a-z]+:\/\//, "");
}

export function BitcoinSettings() {
  return (
    <SettingsGroup title="Bitcoin">
      <ChainSourceCard />
    </SettingsGroup>
  );
}

function ChainSourceCard() {
  // Poll while the Lightning node restarts with a new source.
  const { data, error, mutate } = useSWR<ChainSourceSettings>(KEY, fetcher, {
    refreshInterval: (d) => (d?.restart_pending ? 3000 : 0),
  });

  return (
    <Card>
      <CardHeader>
        <CardTitle>Chain source</CardTitle>
        <CardDescription>Where your Lightning node reads the Bitcoin blockchain.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {error && <Alert tone="danger" title="Could not load the chain source" />}
        {!data && !error && <p className="text-sm text-muted">Loading…</p>}
        {data && !data.editable && (
          <p className="text-sm text-muted">
            Allod does not manage this Lightning node; its chain source is set in ldk-server&apos;s configuration.
          </p>
        )}
        {data?.editable && <SourcePicker data={data} mutate={mutate} />}
      </CardContent>
    </Card>
  );
}

type Choice = "default" | "standby" | "bitcoind" | "custom" | string; // string: preset id

function initialChoice(data: ChainSourceSettings): Choice {
  const r = data.requested;
  if (!r || r.kind === "default") return "default";
  if (r.standby) return "standby";
  if (r.kind === "bitcoind") return "bitcoind";
  return data.presets.find((p) => p.kind === r.kind && p.url === r.url)?.id ?? "custom";
}

function SourcePicker({ data, mutate }: { data: ChainSourceSettings; mutate: KeyedMutator<ChainSourceSettings> }) {
  const r = data.requested;
  const [choice, setChoice] = useState<Choice>(() => initialChoice(data));
  const [kind, setKind] = useState<"electrum" | "esplora">(() => (r?.kind === "electrum" ? "electrum" : "esplora"));
  const [url, setUrl] = useState(() => (initialChoice(data) === "custom" ? (r?.url ?? "") : ""));
  const [rpcAddress, setRpcAddress] = useState(r?.standby ? "" : (r?.rpc_address ?? ""));
  const [rpcUser, setRpcUser] = useState(r?.standby ? "" : (r?.rpc_user ?? ""));
  const [rpcPassword, setRpcPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const defaultName = MODE_NAME[data.default_mode ?? ""] ?? "The stack settings";
  const hasStoredPassword = r?.kind === "bitcoind" && !r.standby;
  const preset = data.presets.find((p) => p.id === choice);

  let picked: ChainSourceRequest | null = null;
  if (choice === "default") picked = { kind: "default" };
  else if (choice === "standby") picked = { kind: "bitcoind", standby: true };
  else if (choice === "bitcoind")
    picked = rpcAddress.trim() && rpcUser.trim() && (rpcPassword || hasStoredPassword)
      ? { kind: "bitcoind", rpc_address: rpcAddress.trim(), rpc_user: rpcUser.trim(), rpc_password: rpcPassword }
      : null;
  else if (choice === "custom") picked = url.trim() ? { kind, url: url.trim() } : null;
  else if (preset) picked = { kind: preset.kind, url: preset.url };

  const unchanged = !!picked && sameRequest(picked, r ?? { kind: "default" });

  async function save() {
    if (!picked) return;
    setErr(null);
    setBusy(true);
    try {
      const saved = picked.standby ? await startStandbyBitcoind() : await saveChainSource(picked);
      await mutate(saved, { revalidate: false });
      setRpcPassword("");
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="space-y-4">
      <CurrentSource data={data} />

      <div className="space-y-2" role="radiogroup" aria-label="Chain source">
        <Option selected={choice === "default"} onSelect={() => setChoice("default")}>
          <span className="flex items-center gap-2 font-medium">
            {defaultName} <Badge>Default</Badge>
          </span>
          <span className="block text-xs text-muted">As configured when the stack was installed (ALLOD_CHAIN)</span>
        </Option>
        {data.standby && (
          <Option selected={choice === "standby"} onSelect={() => setChoice("standby")}>
            <span className="flex items-center gap-2 font-medium">
              Allod&apos;s built-in node {data.standby.enabled && <Badge>Running</Badge>}
            </span>
            <span className="block text-xs text-muted">
              Pruned Bitcoin Core inside this app. A first start downloads the blockchain (hours to days); the
              Lightning node waits for it.
            </span>
          </Option>
        )}
        <Option selected={choice === "bitcoind"} onSelect={() => setChoice("bitcoind")}>
          <span className="font-medium">Your own Bitcoin Core node</span>
          <span className="block text-xs text-muted">Most private: the node checks the chain itself over RPC</span>
        </Option>
        {data.presets.map((p) => (
          <Option key={p.id} selected={choice === p.id} onSelect={() => setChoice(p.id)}>
            <span className="flex items-center gap-2 font-medium">
              {p.name} <Badge>{KIND_NAME[p.kind]}</Badge>
            </span>
            <span className="block break-all font-mono text-xs text-muted">{host(p.url)}</span>
          </Option>
        ))}
        <Option selected={choice === "custom"} onSelect={() => setChoice("custom")}>
          <span className="font-medium">Your own Electrum or Esplora server</span>
          <span className="block text-xs text-muted">Any server you run or trust</span>
        </Option>
      </div>

      {choice === "bitcoind" && (
        <div className="space-y-3 rounded-lg border border-line p-3">
          <div className="space-y-1">
            <Label htmlFor="rpc-address">RPC address</Label>
            <Input id="rpc-address" value={rpcAddress} onChange={(e) => setRpcAddress(e.target.value)} placeholder="192.168.1.10:8332" />
          </div>
          <div className="space-y-1">
            <Label htmlFor="rpc-user">RPC user</Label>
            <Input id="rpc-user" value={rpcUser} onChange={(e) => setRpcUser(e.target.value)} autoComplete="off" />
          </div>
          <div className="space-y-1">
            <Label htmlFor="rpc-password">RPC password</Label>
            <Input
              id="rpc-password"
              type="password"
              value={rpcPassword}
              onChange={(e) => setRpcPassword(e.target.value)}
              autoComplete="new-password"
              placeholder={hasStoredPassword ? "Unchanged" : ""}
            />
          </div>
          <p className="text-xs text-muted">
            The node must be reachable from the Lightning container. A pruned node works only while it still has the
            blocks since your Lightning node last read the chain.
          </p>
        </div>
      )}

      {choice === "custom" && (
        <div className="space-y-3 rounded-lg border border-line p-3">
          <div className="flex gap-2">
            {(["esplora", "electrum"] as const).map((k) => (
              <Button key={k} size="sm" variant={kind === k ? "primary" : "secondary"} onClick={() => setKind(k)}>
                {KIND_NAME[k]}
              </Button>
            ))}
          </div>
          <div className="space-y-1">
            <Label htmlFor="chain-url">Server URL</Label>
            <Input
              id="chain-url"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder={kind === "electrum" ? "ssl://electrum.example.com:50002" : "https://esplora.example.com/api"}
            />
          </div>
        </div>
      )}

      <p className="text-xs text-muted">
        {preset || choice === "custom"
          ? "An external server learns your node's on-chain addresses and its IP address. "
          : ""}
        Saving restarts the Lightning node, which takes about a minute; payments pause meanwhile.
      </p>
      {err && <Alert tone="danger" title="Could not save">{err}</Alert>}
      <Button disabled={busy || !picked || (unchanged && !rpcPassword)} onClick={() => void save()}>
        Use this source
      </Button>
    </div>
  );
}

function sameRequest(a: ChainSourceRequest, b: ChainSourceView): boolean {
  if (a.standby || b.standby) return !!a.standby === !!b.standby;
  return a.kind === b.kind && (a.url ?? "") === (b.url ?? "") && (a.rpc_address ?? "") === (b.rpc_address ?? "") && (a.rpc_user ?? "") === (b.rpc_user ?? "");
}

function describe(a: ActiveChainSource, requested: ChainSourceView | null): string {
  if (a.kind === "bitcoind" && requested?.standby && a.rpc_address === requested.rpc_address) {
    return "Allod's built-in node";
  }
  if (a.kind === "bitcoind") {
    const where = a.mode === "external-bitcoind" ? a.rpc_address : "this stack";
    return `Bitcoin Core${where ? ` · ${where}` : ""}`;
  }
  return `${KIND_NAME[a.kind]} · ${host(a.url ?? "")}`;
}

function CurrentSource({ data }: { data: ChainSourceSettings }) {
  if (data.restart_pending) {
    return (
      <Alert title="Restarting the Lightning node">
        It starts again with the new chain source. This page updates when it is back.
      </Alert>
    );
  }
  if (!data.active) return null;
  return (
    <div className="rounded-lg bg-surface-2 px-3 py-2 text-sm">
      <span className="text-muted">In use: </span>
      <span className="break-all font-medium">{describe(data.active, data.requested)}</span>
    </div>
  );
}

function Option({ selected, onSelect, children }: { selected: boolean; onSelect: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      onClick={onSelect}
      className={cn(
        "w-full rounded-lg border px-4 py-3 text-left text-sm",
        selected ? "border-accent bg-accent-soft/50" : "border-line hover:bg-surface-2",
      )}
    >
      {children}
    </button>
  );
}
