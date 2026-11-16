# File

[API overview](../API.md). [Operations and contracts](../file.hirsel).

Files use core's `HostFile` resource directly. An open file owns its position;
its finalizer closes it and discards the close error, while `close` exposes it.
Read, Write, Append and CreateNew retain the host modes' distinct effects.
Allocating readers return `file.Error` to include Capacity and NotUtf8.
Each operation's `Prefix:` clause names effects that survive error or cancellation.
