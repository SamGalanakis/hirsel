/** Recognition is local presentation only. It never fetches metadata or grants access. */
export interface WebLink {
  url: string;
  label: string;
  kind: "pull-request" | "issue" | "repository" | "web";
  site: string;
}
export function webLink(input: string): WebLink | null {
  // Reject control bytes in untrusted URLs before the browser parser normalizes them.
  // eslint-disable-next-line no-control-regex
  if (!/^https?:\/\//i.test(input) || /[\s\u0000-\u001f\u007f]/u.test(input) || new TextEncoder().encode(input).length > 4096) return null;
  let parsed: URL;
  try { parsed = new URL(input); } catch { return null; }
  if (!parsed.hostname || parsed.username || parsed.password || new TextEncoder().encode(parsed.href).length > 4096) return null;
  const path = parsed.pathname.split("/").filter(Boolean);
  const result: WebLink = { url: parsed.href, label: `${parsed.host}${parsed.pathname === "/" ? "" : parsed.pathname}${parsed.search}${parsed.hash}`, kind: "web", site: parsed.host };
  // Nonstandard ports and lookalike hosts deliberately retain the generic identity.
  if (parsed.port) return result;
  if (parsed.hostname === "github.com" && path.length >= 2 && path.slice(0, 2).every(part => /^[\w.-]+$/.test(part))) {
    const repository = `${path[0]}/${path[1]}`;
    if ((path[2] === "pull" || path[2] === "issues") && /^\d+$/.test(path[3] ?? "")) {
      return { ...result, label: `${repository} · ${path[2] === "pull" ? "PR" : "Issue"} #${path[3]}`, kind: path[2] === "pull" ? "pull-request" : "issue", site: "GitHub" };
    }
    if (path.length === 2) return { ...result, label: repository, kind: "repository", site: "GitHub" };
  }
  return result;
}
export function isBareLinkLabel(label: string, href: string): boolean {
  return label === href || (label.startsWith("www.") && `http://${label}` === href);
}
