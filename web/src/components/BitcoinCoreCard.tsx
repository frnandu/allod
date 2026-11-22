import useSWR from "swr";
import { fetcher, type BitcoindStatus, type ConfirmedBlock, type FeeEstimates, type ProjectedBlock } from "../lib/api";
import { cn } from "../lib/utils";
import { formatAgo, formatEta } from "../lib/eta";
import { useNow } from "../lib/useNow";
import { BlockStrip } from "./BlockStrip";
import { FeeEstimatesSection, feeRate, useFeeEstimates } from "./FeeEstimates";
import { Alert } from "./ui/alert";
import { Badge } from "./ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";

/** Oldest Bitcoin Core major release still receiving security fixes (the last three
 * majors are maintained; a new one ships every ~6 months). Bump with each release. */
const MIN_MAINTAINED_MAJOR = 30;
/** No new block for this long is worth a look (mainnet sees this a few times a year). */
const STALE_TIP_SECS = 2 * 3600;

const CHAIN_NAME: Record<string, string> = {
  main: "mainnet",
  test: "testnet3",
  testnet4: "testnet4",
  signet: "signet",
  regtest: "regtest",
};

export function useBitcoindStatus() {
  return useSWR<BitcoindStatus>("/api/node/bitcoind", fetcher, { refreshInterval: 15000 });
}

/** 310100 → "31.1", 280201 → "28.2.1". */
export function coreVersion(v: number): string {
  const major = Math.floor(v / 10000);
  const minor = Math.floor(v / 100) % 100;
  const patch = v % 100;
  return patch ? `${major}.${minor}.${patch}` : `${major}.${minor}`;
}

export function bytes(value: number) {
  if (value < 1024 ** 2) return `${Math.round(value / 1024).toLocaleString()} KiB`;
  if (value < 1024 ** 3) return `${(value / 1024 ** 2).toFixed(1)} MiB`;
  return `${(value / 1024 ** 3).toFixed(1)} GiB`;
}

/** 466098142 → "466.1 M". */
function millions(n: number) {
  return `${(n / 1e6).toLocaleString(undefined, { maximumFractionDigits: 1, minimumFractionDigits: 1 })} M`;
}

function uptime(secs: number) {
  if (secs < 3600) return `${Math.max(1, Math.floor(secs / 60))} min`;
  if (secs < 86400) return `${Math.floor(secs / 3600)} h ${Math.floor((secs % 3600) / 60)} min`;
  return `${Math.floor(secs / 86400)} d ${Math.floor((secs % 86400) / 3600)} h`;
}

function Stat({ label, value, sub, tone }: { label: string; value: string; sub?: string; tone?: "warn" | "danger" }) {
  return (
    <div>
      <p className="text-muted">{label}</p>
      <p className={tone === "danger" ? "font-mono text-danger" : tone === "warn" ? "font-mono text-warn" : "font-mono"}>{value}</p>
      {sub && <p className="text-xs text-muted">{sub}</p>}
    </div>
  );
}

function Bar({ value, label }: { value: number; label: string }) {
  return (
    <div
      className="h-1.5 overflow-hidden rounded-full bg-surface-2"
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.floor(value * 100)}
    >
      <div className="h-full rounded-full bg-accent transition-[width]" style={{ width: `${Math.max(1, value * 100)}%` }} />
    </div>
  );
}

function BackgroundValidation({
  bg,
  age,
}: {
  bg: NonNullable<BitcoindStatus["background_validation"]>;
  /** Seconds since bitcoind gave these figures: the estimate has run down meanwhile. */
  age: number;
}) {
  const target = bg.target_height;
  if (bg.complete) {
    return (
      <p className="text-sm text-muted">
        <span className="font-medium text-ok">Background validation complete.</span> Every block from 2009
        {target ? ` up to the snapshot at block ${target.toLocaleString()}` : " up to the snapshot"} has been checked:
        this node no longer relies on the snapshot.
      </p>
    );
  }
  const pct = bg.progress != null ? Math.floor(bg.progress * 1000) / 10 : null;
  return (
    <div className="space-y-2 text-sm">
      <div className="flex items-baseline justify-between gap-3">
        <h3 className="font-medium">Background validation</h3>
        <span className="font-mono">{pct != null ? `${pct.toFixed(1)}%` : bg.active ? "running" : "waiting"}</span>
      </div>
      {bg.progress != null && <Bar value={bg.progress} label="Background validation" />}
      {(bg.blocks != null || bg.transactions != null) && (
        <p className="flex flex-wrap gap-x-3 font-mono text-xs text-muted">
          {bg.blocks != null && (
            <span>
              block {bg.blocks.toLocaleString()}
              {target ? ` / ${target.toLocaleString()}` : ""}
            </span>
          )}
          {bg.transactions != null && bg.target_transactions != null && (
            <span>
              {millions(bg.transactions)} / {millions(bg.target_transactions)} transactions
            </span>
          )}
          {bg.progress_basis === "blocks" && <span>progress by blocks</span>}
        </p>
      )}
      {bg.progress_basis === "transactions" && (
        <p className="text-xs">
          <span className="font-medium">
            {formatEta(bg.eta_secs != null ? Math.max(0, bg.eta_secs - age) : null)}
          </span>
          {bg.tx_per_sec != null && (
            <span className="text-muted"> · {Math.round(bg.tx_per_sec).toLocaleString()} transactions/s</span>
          )}
          {bg.eta_secs != null && (
            <span className="text-muted"> · at the recent pace; later years hold bigger transactions</span>
          )}
        </p>
      )}
      <p className="text-xs text-muted">
        This node started from a UTXO snapshot (fast sync) and is already in use. In the background it
        checks every block from 2009 up to the snapshot, so the snapshot no longer has to be trusted. Early
        blocks are nearly empty, so the percentage counts transactions, not blocks. Until it finishes it uses
        extra CPU and disk; nothing to do.
      </p>
    </div>
  );
}

/** The chain around "now" (projected and mined blocks), the fee boxes, and the
 * mempool's figures: everything about what it costs to get into a block. */
function MempoolPanel({
  mempool: m,
  relayFee,
  fees,
  pending,
  mined,
  now,
}: {
  mempool: BitcoindStatus["mempool"] | null | undefined;
  relayFee: number | null;
  fees?: FeeEstimates;
  pending?: ProjectedBlock[] | null;
  mined?: ConfirmedBlock[];
  now: number;
}) {
  const floor = m?.min_fee_sat_vb ?? null;
  // Above the relay fee, the mempool is at its size limit and evicts the cheapest.
  const full = floor != null && relayFee != null && floor > relayFee;
  const fill = m?.usage != null && m.max ? Math.min(1, m.usage / m.max) : null;
  // A block holds about 1 million vbytes.
  const blocks = m?.vbytes != null ? m.vbytes / 1_000_000 : null;
  const strip = (pending?.length ?? 0) > 0 || (mined?.length ?? 0) > 0;
  return (
    <section className="overflow-hidden rounded-xl border border-line bg-surface-2/40">
      <div className="flex items-center justify-between gap-3 border-b border-line px-4 py-2.5">
        <h3 className="font-display text-sm font-medium">Mempool &amp; blocks</h3>
        {full ? (
          <Badge tone="warn">full · evicting cheapest</Badge>
        ) : m?.loaded === false ? (
          <Badge>loading</Badge>
        ) : null}
      </div>
      {strip && (
        <div className="border-b border-line p-4">
          <BlockStrip pending={pending} mined={mined} now={now} />
        </div>
      )}
      {fees && (
        <div className="border-b border-line p-4 last:border-b-0">
          <FeeEstimatesSection fees={fees} floor={floor} />
        </div>
      )}
      {m && (
        <dl className="grid grid-cols-2 gap-x-4 gap-y-3 p-4 text-sm sm:grid-cols-4">
          <div>
            <dt className="text-xs text-muted">Waiting</dt>
            <dd className="font-mono tabular-nums">
              {m.transactions != null ? `${m.transactions.toLocaleString()} tx` : "—"}
            </dd>
            {blocks != null && (
              <p className="text-[11px] text-muted">
                {blocks < 1 ? "less than one block" : `≈ ${blocks.toFixed(blocks < 10 ? 1 : 0)} blocks`}
              </p>
            )}
          </div>
          <div>
            <dt className="text-xs text-muted">Fees waiting</dt>
            <dd className="font-mono text-btc tabular-nums">
              {m.total_fee_sat != null ? `${m.total_fee_sat.toLocaleString()} sat` : "—"}
            </dd>
          </div>
          <div>
            <dt className="text-xs text-muted">Entry floor</dt>
            <dd className={cn("font-mono tabular-nums", full && "text-warn")}>
              {floor != null ? `${feeRate(floor)} sat/vB` : "—"}
            </dd>
          </div>
          {fill != null && (
            <div>
              <dt className="text-xs text-muted">Memory</dt>
              <dd className="font-mono text-xs tabular-nums">
                {bytes(m.usage!)} / {bytes(m.max!)}
              </dd>
              <div
                className="mt-1 h-1.5 overflow-hidden rounded-full bg-surface-2"
                role="progressbar"
                aria-label="Mempool memory"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={Math.round(fill * 100)}
              >
                <div
                  className={cn("h-full rounded-full", full ? "bg-warn" : "bg-accent/70")}
                  style={{ width: `${Math.max(1, fill * 100)}%` }}
                />
              </div>
            </div>
          )}
        </dl>
      )}
    </section>
  );
}

/** The Node page's Bitcoin Core block: sync, background validation, peers, mempool,
 * disk, warnings and fee estimates. Hidden in light modes (no bitcoind of Allod's own). */
export function BitcoinCoreCard() {
  const { data: s } = useBitcoindStatus();
  const { data: fees } = useFeeEstimates();
  const now = useNow();
  if (!s?.source && !fees?.source) return null;

  const reachable = !!s?.source && !s.error;
  const synced = reachable && s.initial_block_download === false;
  const regtest = s?.chain === "regtest";
  const tipAge = s?.best_block_time ? now - s.best_block_time : null;
  const staleTip = synced && !regtest && tipAge != null && tipAge > STALE_TIP_SECS;
  const peers = s?.peers;
  const noPeers = reachable && !regtest && peers?.total === 0;
  const networkOff = reachable && peers?.network_active === false;
  const major = s?.version ? Math.floor(s.version / 10000) : null;
  const outdated = major != null && major < MIN_MAINTAINED_MAJOR;
  const figuresAge = s?.updated_at ? now - s.updated_at : 0;

  const badge: { tone: "ok" | "warn" | "danger" | "accent"; label: string } = !reachable
    ? { tone: "danger", label: s?.source ? "Unreachable" : "Unknown" }
    : !synced
      ? {
          tone: "accent",
          label: `Syncing${s.verification_progress != null ? ` ${(Math.floor(s.verification_progress * 1000) / 10).toFixed(1)}%` : ""}`,
        }
      : networkOff || noPeers
        ? { tone: "danger", label: "No peers" }
        : staleTip
          ? { tone: "warn", label: "Tip behind" }
          : s.background_validation?.active
            ? { tone: "accent", label: "Validating history" }
            : { tone: "ok", label: "Synced" };

  const subtitle = [
    s?.version ? `Bitcoin Core ${coreVersion(s.version)}` : "Bitcoin Core",
    s?.chain ? (CHAIN_NAME[s.chain] ?? s.chain) : null,
    s?.pruned ? "pruned" : s?.pruned === false ? "full archive" : null,
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <Card className="card-wash-bitcoin min-w-0 lg:col-span-2">
      <CardHeader className="flex-row items-center justify-between gap-3">
        <div>
          <CardTitle>Bitcoin node</CardTitle>
          <p className="pt-1 text-xs text-muted">{subtitle}</p>
        </div>
        <Badge tone={badge.tone}>{badge.label}</Badge>
      </CardHeader>
      <CardContent className="space-y-5">
        {s?.source && s.error ? (
          <Alert tone="danger" title="Bitcoin Core does not answer">
            {s.error}
          </Alert>
        ) : (
          reachable && (
            <>
              {!synced && s.verification_progress != null && (
                <Bar value={s.verification_progress} label="Initial block download" />
              )}
              <div className="grid grid-cols-2 gap-x-4 gap-y-4 text-sm sm:grid-cols-4">
                <Stat
                  label="Block height"
                  value={s.blocks != null ? s.blocks.toLocaleString() : "—"}
                  sub={
                    s.headers != null && s.blocks != null && s.headers > s.blocks
                      ? `${(s.headers - s.blocks).toLocaleString()} behind headers`
                      : undefined
                  }
                />
                <Stat
                  label="Last block"
                  value={tipAge != null ? (tipAge < 60 ? "just now" : `${formatAgo(tipAge)} ago`) : "—"}
                  tone={staleTip ? "warn" : undefined}
                />
                <Stat
                  label="Peers"
                  value={networkOff ? "network off" : peers?.total != null ? String(peers.total) : "—"}
                  sub={peers && peers.outbound != null ? `${peers.outbound} out · ${peers.inbound ?? 0} in` : undefined}
                  tone={networkOff || noPeers ? "danger" : undefined}
                />
                <Stat
                  label="Disk"
                  value={s.size_on_disk != null ? bytes(s.size_on_disk) : "—"}
                  sub={
                    s.pruned
                      ? s.prune_height != null
                        ? `blocks from ${s.prune_height.toLocaleString()}`
                        : "pruned"
                      : undefined
                  }
                />
              </div>
              {s.background_validation && <BackgroundValidation bg={s.background_validation} age={figuresAge} />}
              {(s.warnings ?? []).map((w) => (
                <Alert key={w} tone="warn" title="Bitcoin Core warns">
                  {w}
                </Alert>
              ))}
              {outdated && (
                <Alert tone="warn" title={`Bitcoin Core ${coreVersion(s.version!)} no longer gets security fixes`}>
                  Only the last three major releases are maintained. Update the Bitcoin node this wallet
                  relies on.
                </Alert>
              )}
              {networkOff ? (
                <Alert tone="danger" title="Bitcoin Core's networking is turned off">
                  It sees no new blocks or transactions; the Lightning node cannot notice channel closes in time.
                </Alert>
              ) : (
                noPeers && (
                  <Alert tone="danger" title="Bitcoin Core has no peers">
                    Without peers it sees no new blocks: the Lightning node cannot notice channel closes in time.
                    Check the machine's internet connection and firewall.
                  </Alert>
                )
              )}
              {staleTip && !noPeers && !networkOff && (
                <Alert tone="warn" title={`No new block for ${formatAgo(tipAge!)}`}>
                  This happens on the Bitcoin network now and then. If it lasts, the node may be cut off from
                  honest peers or its clock may be wrong.
                </Alert>
              )}
            </>
          )
        )}
        {(fees?.source || (reachable && s.mempool)) && (
          <MempoolPanel
            mempool={reachable ? s.mempool : null}
            relayFee={s?.relay_fee_sat_vb ?? null}
            fees={fees?.source ? fees : undefined}
            pending={reachable ? s.mempool_blocks : null}
            mined={reachable ? s.recent_blocks : undefined}
            now={now}
          />
        )}
        {reachable && (
          <p className="text-xs text-muted">
            {[
              s.subversion,
              s.uptime != null ? `up ${uptime(s.uptime)}` : null,
              peers?.reachable?.length ? `networks: ${peers.reachable.join(", ")}` : null,
              figuresAge >= 60 ? `bitcoind busy, figures from ${formatAgo(figuresAge)} ago` : null,
            ]
              .filter(Boolean)
              .join(" · ")}
          </p>
        )}
      </CardContent>
    </Card>
  );
}
