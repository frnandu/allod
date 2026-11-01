import useSWR from "swr";
import { fetcher, type InfoResponse } from "../lib/api";
import { shorten } from "../lib/utils";

/** Explorer link for a pubkey from the `{pubkey}` template; only http(s) links. */
export function explorerLink(template: string | undefined, pubkey: string): string | null {
  if (!template || !/^https?:\/\//i.test(template)) return null;
  return template.replace("{pubkey}", encodeURIComponent(pubkey));
}

/**
 * A node's alias (from the node's own gossip graph) above its shortened pubkey. The alias
 * links to the configured node explorer (ALLOD_NODE_EXPLORER_URL, default amboss.space);
 * without an alias, the pubkey is the link.
 */
export function NodeLabel({ pubkey, alias, head = 12, tail = 8 }: { pubkey: string; alias?: string | null; head?: number; tail?: number }) {
  const { data: info } = useSWR<InfoResponse>("/api/info", fetcher, { revalidateOnFocus: false });
  const href = explorerLink(info?.deployment.node_explorer_url, pubkey);
  const link = (text: string, className: string) =>
    href ? (
      <a href={href} target="_blank" rel="noopener noreferrer" className={`${className} hover:underline`} title="Open in node explorer">
        {text}
      </a>
    ) : (
      <span className={className}>{text}</span>
    );
  const short = shorten(pubkey, head, tail);
  return (
    <span className="inline-flex min-w-0 flex-col" title={pubkey}>
      {alias ? (
        <>
          {link(alias, "truncate text-sm font-medium")}
          <span className="font-mono text-xs text-muted">{short}</span>
        </>
      ) : (
        link(short, "font-mono text-xs")
      )}
    </span>
  );
}
