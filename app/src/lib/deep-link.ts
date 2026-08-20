// `/t/<id>` — one Task, addressable.
//
// The route is the ref in URL form, and it is not a hidden feature: the address
// bar tracks focus, so opening `#12` puts `/t/12` in it and the browser's own
// copy-link is the deep link. That is why the ref tag on the Task card copies
// the bare `#12` instead: the short form is the one thing the browser cannot
// give you, and it is the form that cites the Task back in the composer.
//
// Focus changes push history, so Back walks back through the Tasks you opened —
// the same contract every issue tracker has. Ambient is `/`, because ambient is
// the absence of focus, not a place.

/** The path a focus state is addressed by. */
export function taskPath(id: number | null): string {
  return id === null ? "/" : `/t/${id}`;
}

/** The Task a path names, or null for anything else (including `/`). */
export function taskIdFromPath(pathname: string): number | null {
  const match = /^\/t\/(\d+)\/?$/.exec(pathname);
  if (!match) return null;
  const id = Number.parseInt(match[1], 10);
  return Number.isSafeInteger(id) && id > 0 ? id : null;
}
