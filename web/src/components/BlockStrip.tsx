import { type ReactNode, useId, useLayoutEffect, useRef, useState } from "react";
import type { ConfirmedBlock, ProjectedBlock } from "../lib/api";
import { formatAgo } from "../lib/eta";
import { feeRate } from "./FeeEstimates";

/** A block holds about a million vbytes. */
const BLOCK_VSIZE = 1_000_000;

/** Fee colours as block explorers use them: green for cheap blocks through yellow,
 * orange and red to purple for very expensive ones (sat/vB, log scale between stops). */
const FEE_STOPS: [number, string][] = [
  [0.1, "#3f6a1c"],
  [1, "#5a8a1d"],
  [3, "#8c9a19"],
  [8, "#b6921b"],
  [20, "#c76d1e"],
  [50, "#bf3f2c"],
  [150, "#a52b66"],
  [400, "#782d91"],
];

export function feeColor(rate: number | null | undefined): string {
  if (rate == null) return "#4b5263";
  const r = Math.max(rate, FEE_STOPS[0]![0]);
  for (let i = 1; i < FEE_STOPS.length; i++) {
    const [hi, hc] = FEE_STOPS[i]!;
    if (r <= hi) {
      const [lo, lc] = FEE_STOPS[i - 1]!;
      const t = (Math.log(r) - Math.log(lo)) / (Math.log(hi) - Math.log(lo));
      return `color-mix(in oklab, ${hc} ${Math.round(t * 100)}%, ${lc})`;
    }
  }
  return FEE_STOPS[FEE_STOPS.length - 1]![1];
}

/** Mined blocks: one blue-violet look, whatever their fees. */
const MINED = { from: "#3d4fc4", to: "#8a3fcf" };
/** The empty part of a block not filled to the limit. */
const EMPTY = "#3b4150";

function megabytes(bytes: number) {
  return `${(bytes / 1_000_000).toFixed(2)} MB`;
}

/** Mined blocks' rates are whole sat/vB (as Bitcoin Core gives them): no decimals. */
function rate(r: number, whole?: boolean) {
  return whole ? Math.round(r).toLocaleString() : feeRate(r);
}

/** Cube geometry (px): front face size, depth of the oblique top and side faces, and
 * the offset of the block drawn behind a stack. */
const FACE = 112;
const DEPTH = 10;
const STACK = 7;
const BOX = STACK + DEPTH + FACE;

/**
 * One block as a cube in oblique projection, depth going up and to the left: the front
 * face fills from the bottom by how full the block is, the top face is lit, the left
 * face shaded, both following the fill. `stacked` draws a second block behind it.
 */
function Cube({
  top,
  fill,
  color,
  stacked,
  children,
  label,
}: {
  top: string;
  fill: number;
  /** Solid fee colour, or the mined gradient. */
  color: string | typeof MINED;
  stacked?: boolean;
  children: ReactNode;
  label: string;
}) {
  const id = useId().replace(/:/g, "");
  const paint = typeof color === "string" ? color : `url(#g${id})`;
  // A mined or projected block is never filled to the last vbyte: treat 97% as full,
  // or nearly every cube would show a sliver of empty space and an empty-coloured top.
  const f = fill >= 0.97 ? 1 : Math.max(0.04, fill);
  const cube = (x: number, y: number, back?: boolean) => {
    const W = FACE, d = DEPTH, H = FACE;
    const h = H * f;
    // Corners: front face (x+d, y+d)..(x+d+W, y+d+H); back edge d up and left.
    const top = `${x},${y} ${x + W},${y} ${x + W + d},${y + d} ${x + d},${y + d}`;
    const side = (y0: number, y1: number) =>
      `${x},${y + y0} ${x + d},${y + d + y0} ${x + d},${y + d + y1} ${x},${y + y1}`;
    return (
      <g opacity={back ? 0.45 : 1}>
        {/* Front: empty part, then the filled part from the bottom. */}
        <rect x={x + d} y={y + d} width={W} height={H} fill={EMPTY} />
        <rect x={x + d} y={y + d + H - h} width={W} height={h} fill={paint} />
        {/* Left side: same split, shaded. */}
        <polygon points={side(0, H - h)} fill={EMPTY} />
        <polygon points={side(H - h, H)} fill={paint} />
        <polygon points={side(0, H)} fill="black" opacity={0.32} />
        {/* Top: the fill's colour once the block is full, else the empty part's; lit. */}
        <polygon points={top} fill={f >= 1 ? paint : EMPTY} />
        <polygon points={top} fill="white" opacity={0.22} />
      </g>
    );
  };
  return (
    <li className="flex shrink-0 flex-col items-center gap-1" style={{ width: BOX }} aria-label={label}>
      <p className="font-mono text-xs text-muted" style={{ paddingLeft: STACK + DEPTH }}>
        {top}
      </p>
      <div className="relative" style={{ width: BOX, height: BOX }}>
        <svg aria-hidden width={BOX} height={BOX} className="absolute inset-0 drop-shadow-sm">
          {typeof color !== "string" && (
            <defs>
              <linearGradient id={`g${id}`} x1="0" y1="1" x2="1" y2="0">
                <stop offset="0" stopColor={color.from} />
                <stop offset="1" stopColor={color.to} />
              </linearGradient>
            </defs>
          )}
          {stacked && cube(0, 0, true)}
          {cube(STACK, STACK)}
        </svg>
        <div
          className="absolute flex flex-col items-center justify-center space-y-0.5 px-1.5 text-center text-white [text-shadow:0_1px_2px_rgb(0_0_0/0.4)]"
          style={{ left: STACK + DEPTH, top: STACK + DEPTH, width: FACE, height: FACE }}
        >
          {children}
        </div>
      </div>
    </li>
  );
}

function Rates({
  median,
  min,
  max,
  whole,
}: {
  median: number | null;
  min: number | null;
  max: number | null;
  whole?: boolean;
}) {
  const range = min != null && max != null ? `${rate(min, whole)} – ${rate(max, whole)}` : null;
  return (
    <>
      <p className="font-mono text-base font-medium leading-tight">
        {median != null ? `~${rate(median, whole)}` : "—"}
        <span className="pl-1 font-sans text-[10px] font-normal opacity-80">sat/vB</span>
      </p>
      {range && <p className="font-mono text-[10px] leading-tight opacity-80">{range}</p>}
    </>
  );
}

function Pending({ block, index }: { block: ProjectedBlock; index: number }) {
  const many = block.count > 1;
  const when = many ? `+${block.count} blocks` : index === 0 ? "next block" : `in ~${(index + 1) * 10} min`;
  return (
    <Cube
      top={many ? `+${block.count}` : index === 0 ? "next" : `+${index}`}
      fill={many ? 1 : block.vsize / BLOCK_VSIZE}
      color={feeColor(block.median_rate)}
      stacked={many}
      label={`Projected block ${index + 1}: ${when}`}
    >
      <Rates median={block.median_rate} min={block.min_rate} max={block.max_rate} />
      <p className="pt-1 text-[11px] leading-tight">{megabytes(block.vsize)}</p>
      <p className="text-[11px] leading-tight opacity-80">{when}</p>
    </Cube>
  );
}

function Mined({ block, now }: { block: ConfirmedBlock; now: number }) {
  const age = now - block.time;
  return (
    <Cube
      top={block.height.toLocaleString()}
      fill={(block.vsize ?? block.size) / BLOCK_VSIZE}
      color={MINED}
      label={`Block ${block.height.toLocaleString()}`}
    >
      <Rates median={block.median_rate} min={block.min_rate} max={block.max_rate} whole />
      <p className="pt-1 text-[11px] leading-tight">{block.txs.toLocaleString()} tx</p>
      <p className="text-[11px] leading-tight opacity-80">{age < 60 ? "just now" : `${formatAgo(age)} ago`}</p>
    </Cube>
  );
}

/**
 * The chain at a glance, the way block explorers draw it: one row that scrolls as a
 * whole, blocks the mempool would make next left of the "now" line (the next one against
 * it) and mined blocks right of it (the newest against it). The line starts in the
 * middle; spacers make that possible however few blocks one side has. It is centred
 * again on resize and new data until the viewer scrolls themselves.
 */
export function BlockStrip({
  pending,
  mined,
  now,
}: {
  pending: ProjectedBlock[] | null | undefined;
  mined: ConfirmedBlock[] | undefined;
  now: number;
}) {
  const p = pending ?? [];
  const m = mined ?? [];
  const scroller = useRef<HTMLDivElement>(null);
  const left = useRef<HTMLOListElement>(null);
  const line = useRef<HTMLDivElement>(null);
  const right = useRef<HTMLOListElement>(null);
  const touched = useRef(false);
  const [pad, setPad] = useState({ l: 0, r: 0 });

  // Spacers: room to put the line in the middle when a side is narrower than half.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el || !left.current || !line.current || !right.current) return;
    const fit = () => {
      const half = el.clientWidth / 2;
      const lineHalf = line.current!.offsetWidth / 2;
      const l = Math.max(0, Math.round(half - lineHalf - left.current!.offsetWidth));
      const r = Math.max(0, Math.round(half - lineHalf - right.current!.offsetWidth));
      setPad((old) => (old.l === l && old.r === r ? old : { l, r }));
    };
    fit();
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(fit);
    [el, left.current, right.current].forEach((n) => ro.observe(n));
    return () => ro.disconnect();
  }, []);

  // The line in the middle, unless the viewer has scrolled.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el || !line.current || touched.current) return;
    el.scrollLeft = line.current.offsetLeft + line.current.offsetWidth / 2 - el.clientWidth / 2;
  }, [pad, p.length, m.length]);

  if (p.length === 0 && m.length === 0) return null;
  const mark = () => {
    touched.current = true;
  };
  return (
    <div>
      <div
        ref={scroller}
        onWheel={mark}
        onPointerDown={mark}
        onTouchStart={mark}
        onKeyDown={mark}
        tabIndex={0}
        aria-label="Blocks: mempool and mined"
        className="relative overflow-x-auto overflow-y-hidden pb-2 outline-none [mask-image:linear-gradient(to_right,transparent,black_28px,black_calc(100%-28px),transparent)] [scrollbar-width:thin]"
      >
        <div className="flex w-max items-end">
          <div aria-hidden style={{ width: pad.l }} className="shrink-0" />
          {/* row-reverse: the next block sits against the line, later ones further left. */}
          <ol ref={left} className="flex flex-row-reverse items-end gap-2 pl-4" aria-label="Mempool blocks">
            {p.length === 0 ? (
              <li className="flex size-28 shrink-0 items-center justify-center border border-dashed border-line text-xs text-muted">
                empty
              </li>
            ) : (
              p.map((b, i) => <Pending key={i} block={b} index={i} />)
            )}
          </ol>
          <div ref={line} aria-hidden className="mx-4 flex flex-col items-center gap-1 self-stretch">
            <p className="text-[10px] uppercase tracking-wider text-muted">now</p>
            <div className="w-px flex-1 border-l-2 border-dashed border-muted/50" />
          </div>
          <ol ref={right} className="flex items-end gap-2 pr-4" aria-label="Recent blocks">
            {m.map((b) => (
              <Mined key={b.hash} block={b} now={now} />
            ))}
          </ol>
          <div aria-hidden style={{ width: pad.r }} className="shrink-0" />
        </div>
      </div>
      <div className="grid grid-cols-[minmax(0,1fr)_2.5rem_minmax(0,1fr)] pt-1 text-[11px] text-muted">
        <p className="text-right">
          ← mempool<span className="hidden sm:inline">, as a miner would fill blocks now</span>
        </p>
        <span />
        <p>mined →</p>
      </div>
    </div>
  );
}
