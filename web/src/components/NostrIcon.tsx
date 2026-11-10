export function NostrIcon({ size = 16 }: { size?: number }) {
  return <img src="/nostr.png" alt="" aria-hidden="true" width={size} height={size} className="inline-block shrink-0 object-contain" />;
}
