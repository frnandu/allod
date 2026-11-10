/** Suggest a valid, unused username on one domain. */
export function suggestIdentityUsername(name: string, taken: string[]): string {
  const base = name.normalize("NFKD")
    .replace(/[\u0300-\u036f]/g, "")
    .toLowerCase()
    .trim()
    .replace(/\s+/g, "-")
    .replace(/[^a-z0-9._-]/g, "")
    .replace(/^[._-]+|[._-]+$/g, "")
    .slice(0, 64) || "account";
  const used = new Set(taken.map((value) => value.toLowerCase()));
  for (const reserved of ["admin", "administrator", "root", "allod", "api", "www", "mail", "support", "help", "info", "owner", "wallet", "main", "lnurlp", "well-known", "postmaster", "hostmaster", "abuse", "security", "noreply", "_"]) used.add(reserved);
  if (!used.has(base)) return base;
  for (let n = 2; ; n += 1) {
    const suffix = `-${n}`;
    const candidate = `${base.slice(0, 64 - suffix.length)}${suffix}`;
    if (!used.has(candidate)) return candidate;
  }
}
