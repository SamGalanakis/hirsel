const SAFE_SCHEMES = ["http:", "https:", "mailto:"];

/** A safe href, or null when the URL should degrade to plain text. */
export function safeHref(url: string | null | undefined): string | null {
  // Control characters and backslashes must not disguise a URL scheme or authority.
  // eslint-disable-next-line no-control-regex
  if (!url || /[\u0000-\u001f\u007f\\]/u.test(url)) return null;
  const trimmed = url.trim();
  // Relative and anchor links carry no scheme and are harmless.
  if (/^[#/](?![/\\])/.test(trimmed)) return trimmed;
  try {
    const parsed = new URL(trimmed, "https://hirsel.invalid/");
    return SAFE_SCHEMES.includes(parsed.protocol) && !parsed.username && !parsed.password ? trimmed : null;
  } catch {
    return null;
  }
}
