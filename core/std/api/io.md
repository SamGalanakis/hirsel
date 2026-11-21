# Io

[API overview](../API.md). [Process](process.md). [Stdin](stdin.md). [Operations and contracts](../io.hirsel).

Buffered I/O uses explicit host capabilities. `Lines` owns its input source and
lexical line views freeze its buffer. `BufferedInput` reads raw bytes into a
reusable buffer. `read_until_lf` appends through the delimiter or EOF.
`Out` exposes `pending` and explicit
`flush`; it performs no hidden close or flush work. Source contracts specify
allocation, buffer growth and partial host effects for each operation.

**Input sources.** `pub type Input = File(HostFile) | Stdin(Stdin)` is the one source
type for every reader in this module. `lines(take src: Input, cap)` and
`buffered_input(take src: Input, cap)` take it, and on `Capacity` they return the
source unchanged as `(Capacity, Input)`. A `File` source reads through `host.read`. A
`Stdin` source reads through `stdin.read` with deadline `Never`, so it is a
cancellation point while it waits. `io.Error` is
`Io(IoError) | Stdin(StdinError) | Capacity(Capacity) | NotUtf8`, and each arm comes
from the source that produced it. `read_all(i: Stdin) -> Result[Bytes, Error] waits`
reads to end of input. It grows by GROWTH from 4096 bytes under the current limit, as
`file.read` does, and on failure the bytes read so far are cleaned and refunded.
