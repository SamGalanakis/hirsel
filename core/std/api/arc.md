# Arc

[API overview](../API.md). [Operations and contracts](../arc.hirsel).

This module extends core's `arc` operations with `update`, a copy-on-write
operation over `Arc[T]` for `T: shareable + send`. The scoped callback runs once
on the payload through a mutable view and returns its result.

`update` first calls `arc.with_unique`. A unique Arc is updated in place without
copying its payload. When the Arc is shared, `update` clones the payload, wraps
the clone in a new Arc, replaces the caller's holder, and tries unique access
again. Other holders retain their original payload.

If cloning or the new Arc's allocation returns Capacity, the caller's holder
is unchanged and the callback has not run. The callback is scoped and cannot
retain the mutable view beyond its call.
