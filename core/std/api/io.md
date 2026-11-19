# Io

[API overview](../API.md). [Process](process.md). [Operations and contracts](../io.hirsel).

Buffered I/O uses explicit host capabilities. `Lines` owns its HostFile and
lexical line views freeze its buffer. `BufferedInput` reads raw bytes into a
reusable buffer. `read_until_lf` appends through the delimiter or EOF.
`Out` exposes `pending` and explicit
`flush`; it performs no hidden close or flush work. Source contracts specify
allocation, buffer growth and partial host effects for each operation.
