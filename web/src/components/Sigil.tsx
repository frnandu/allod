import { useMemo } from "react";

/**
 * Account sigil (SPEC §13.2): an abstract pattern of arcs derived from a pubkey or
 * any hex/text seed. Deterministic, no network requests.
 */
export function Sigil({ seed, size = 32 }: { seed: string; size?: number }) {
  const arcs = useMemo(() => {
    const bytes: number[] = [];
    for (let i = 0; i < 32; i++) {
      const c = seed.charCodeAt(i % Math.max(seed.length, 1)) || 0;
      bytes.push((c * 131 + i * 17 + (bytes[i - 1] ?? 7)) % 256);
    }
    const hue = 160 + (bytes[0]! % 40); // teal family
    return [0, 1, 2, 3].map((i) => {
      const b = bytes.slice(i * 6, i * 6 + 6);
      const r = 5 + i * 3.2;
      const start = (b[0]! / 255) * Math.PI * 2;
      const sweep = Math.PI * (0.6 + (b[1]! / 255) * 1.2);
      const x1 = 16 + r * Math.cos(start);
      const y1 = 16 + r * Math.sin(start);
      const x2 = 16 + r * Math.cos(start + sweep);
      const y2 = 16 + r * Math.sin(start + sweep);
      const large = sweep > Math.PI ? 1 : 0;
      const color = i === 3 && b[2]! % 3 === 0 ? "#f7931a" : `hsl(${hue + i * 8} 70% ${48 + i * 6}%)`;
      return { d: `M ${x1} ${y1} A ${r} ${r} 0 ${large} 1 ${x2} ${y2}`, color, w: 1.6 + (b[3]! % 3) * 0.5 };
    });
  }, [seed]);
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" className="shrink-0 rounded-full bg-surface-2" aria-hidden="true">
      {arcs.map((a, i) => (
        <path key={i} d={a.d} fill="none" stroke={a.color} strokeWidth={a.w} strokeLinecap="round" />
      ))}
      <circle cx="16" cy="16" r="1.8" className="fill-fg/70" />
    </svg>
  );
}
