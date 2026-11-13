# files: hand derivation

The input is hello, space, world, newline: 12 bytes. Byte sums are
532+32+552+10=1126. Open source, create destination, acquire the real exclusive
lock, reserve the buffer, read 12 bytes, write 12 and then read EOF. Record
copied=12. Cleanup unlocks/closes lock, closes destination, closes source.
A fresh open and try_lock succeeds. Read destination and report 12/1126.
For error-work write exactly the first min(17,n) bytes before returning Io@copy;
that external prefix survives. Cleanup-report injections happen after the real
release. Keep an existing work error primary, then append lock, destination,
source causes; otherwise remove the first cleanup cause as primary.

```text
acquire=source
acquire=destination
acquire=lock
copied=12
release=lock
release=destination
release=source
destination-bytes=12 sum=1126 lock-free=true
```

## Logical charges

| site | either entry |
|---|---|
| copy.buffer | exactly List[u8] capacity 4096 = 4096 |
| setup, error-second only | b"existing": List[u8] capacity 8 = 8 |

HostFile acquisition is mechanical and admits no cause cell. By MEM-24,
std.file.try_lock admits a Lock block of HostFile(8)+cause(32)=40 before the OS
call (file.hirsel's Alloc: 40). Three traced Owned resources
reserve their declared finalizer grants of 1024 each, plus fields and cause
cells. Their Option placeholders allow consuming close/unlock without a partial
move from a resource. Finalizers allocate only from those grants and never
admit from an already exhausted ambient limit. The 16-item trace channel stores
Event's inline size 8+String-header(24)=32, so charges 16×32+64=576.
A finalizer can send one release line, one error line and one cleanup cause;
16 events cover the three acquisitions and all three failing cleanups.
The matched entry calls the identical three-handle implementation, as both
Rust twins do. Temp removes the private directory on every exit edge.

All charges use MEM-24, charge-v1. Empty owners and string literals charge 0;
a byte-string literal admits its length as List[u8] storage.
A growth block has capacity max(needed,4,2×old), and is admitted before the old
block is refunded. Named injections are synthetic source sites, not host-OOM
or allocator-call counters. Output and instrumentation charges are separate
from the payload charges above. These are hand derivations, not executed Hirsel
results.

Resource admission follows 77ad169e. A refused Temp returns its parent/name,
so the already-created directory is removed before Capacity propagates. Other
resource literals pass their returned fields through result.capacity_only.
For 9175e560, LockError.Capacity returns the unacquired HostFile. lock_capacity
moves it into a local scope: its mechanical, shielded cleanup closes it exactly
once before the original Capacity is returned. No lock was held, so no lock
acquire/release event is invented. Already-owned destination and source then clean
in the usual order. This decision applies to acquisition, setup and probe calls.

Sender and Receiver have logical width 24 (core.md), so each traced Owned block
holds Option[HostFile](16)+Option[Lock](16)+String(24)+bool(1)+Sender(24),
plus its cause cell 32 and finalizer grant 1024, totaling 1137. This resource row
says fields+cell+grant, without an engine alignment charge. Temp holds Dir(8),
String(24) and cause(32), totaling 64; its name block is separate.

## Source length

Physical lines, including comments and blanks: 222 idiomatic and 4 matched;
Rust entry plus shared program.rs: 175 idiomatic and 175 matched. The matched
entry calls the same 222-line implementation, so its effective total is 226.
Shared support is counted separately in REPORT.md.
