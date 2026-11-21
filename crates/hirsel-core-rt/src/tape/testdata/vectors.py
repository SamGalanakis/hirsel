"""Independent oracle for the tape laws (REPLAY-SPEC sections 4 and 5, tape format 6).

Imports no compiler or runtime code: it encodes the spec's byte format by hand
and checks blocks with the system libxxhash. Run from anywhere:

    python3 -I crates/hirsel-core-rt/src/tape/testdata/vectors.py

It writes, next to itself:
- xxh3.bin: 265 rows (u64 length, u64 XXH3_64) over check.hirsel's image, and
  asserts they are check.hirsel's own HASHES;
- hand-tape.bin: the tape the runtime's Record must write for the scripted run
  in tape/tests.rs (`scripted_run`), every value chosen here by hand;
- grant-gap-tape.bin and model-f4-tape.bin: the positioned grant and wait
  example and the two-clock model;
- net-tape.bin: a Run with duplicate Net rules, socket outcomes and DATA.
"""
import ctypes, pathlib, re, struct

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[4]
x = ctypes.CDLL('libxxhash.so.0')
x.XXH3_64bits.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
x.XXH3_64bits.restype = ctypes.c_uint64
def xxh3(b): return x.XXH3_64bits(bytes(b), len(b))

# ---- xxh3 vectors: the image of core/std/tests/check.hirsel ----
image = bytes([77] * 11) + bytes((i * 17 + 3) & 0xff for i in range(4096)) + bytes([99] * 13)
text = (ROOT / 'core/std/tests/check.hirsel').read_text()
lengths = [int(v) for v in re.findall(r'(\d+)u64', re.search(r'LENGTHS[^=]*=\s*\[([^\]]*)\]', text).group(1))]
hashes = [int(v, 16) for v in re.findall(r'0x([0-9a-f]+)u64', re.search(r'HASHES[^=]*=\s*\[([^\]]*)\]', text).group(1))]
assert len(lengths) == len(hashes) == 265
rows = b''
for n, h in zip(lengths, hashes):
    assert xxh3(image[11:11 + n]) == h, n
    rows += struct.pack('<QQ', n, h)
(HERE / 'xxh3.bin').write_bytes(rows)

# ---- the format, by hand from section 5 ----
def vi(n):
    out = bytearray()
    while n >= 128: out.append((n & 127) | 128); n >>= 7
    out.append(n); return bytes(out)
def zz(d): return (d << 1) if d >= 0 else ((-d) << 1) - 1
def lb(b): return vi(len(b)) + b

TAPE_ID = bytes(range(0x10, 0x20))
BUILD = bytes(range(0x20, 0x40))
def block(seq, flags, payload):
    head = bytes([flags]) + b'HTB' + struct.pack('<QI', seq, len(payload))
    return head + payload + struct.pack('<Q', xxh3(TAPE_ID + head + payload))

# Header: Call tape (no header grants).
header = (struct.pack('<HB', 6, 2) + TAPE_ID + lb(b'app@1/main/run') + BUILD
          + struct.pack('<IQQ', 0, 1000, 0) + bytes([0]) + lb(b'arg') + vi(0))

# Op tags (provider.rs) and grant kinds (entry.rs).
OPEN, READ, CLOSE, MKDTEMP, WALL_MS, MONO_NS, STDOUT, RENAME, SLEEP_UNTIL, SYNC_DIR = 0, 1, 3, 7, 10, 11, 12, 14, 15, 17
LISTEN, ACCEPT, SEND_PAIR, RECV_BOUNDED = 20, 21, 26, 27
DIR, NET, CLOCK, K_STDOUT, ARGS = 0, 1, 2, 4, 10
WRITES = {2, 12, 13}
PAYLOAD = {READ, MKDTEMP, LISTEN, ACCEPT, RECV_BOUNDED}

def args(a):
    return lb(a) if len(a) <= 16 else bytes([17]) + struct.pack('<Q', xxh3(a))

class Tape:
    def __init__(s):
        s.blocks = [block(0, 0, header)]; s.buf = b''; s.task = None; s.prev = {}
    def close(s, flags=0):
        s.blocks.append(block(len(s.blocks), flags, s.buf)); s.buf = b''; s.task = None
    def put(s, frame, data=()):
        s.buf += frame
        for d in data:
            if len(s.buf) >= 65536: s.close()
            s.buf += d
        if len(s.buf) >= 65536: s.close()
    def grant(s, kind, vector=None, gap=0):
        s.put(bytes([7]) + vi(gap) + bytes([kind]) + (lb(vector) if kind == ARGS else b''))
    def op(s, op, obj, a, value=0, error=None, written=None, payload=b''):
        head = 3
        f = b''
        if s.task != 0: head |= 0x10; f += vi(0); s.task = 0
        f = bytes([head]) + f + bytes([op]) + vi(obj) + args(a)
        if op in (WALL_MS, MONO_NS):
            prev = s.prev.get((op, obj), 0); s.prev[(op, obj)] = value; value = zz(value - prev)
        f += bytes([1 if error else 0]) + vi(value)
        if error: f += bytes([error[0]]) + vi(error[1])
        if op in WRITES: f += vi(written)
        data = []
        if op in PAYLOAD:
            f += lb(payload[:65536])
            data = [bytes([9]) + lb(payload[i:i + 65536]) for i in range(65536, len(payload), 65536)]
        s.put(f, data)
    def end(s, kind, outcome):
        # v2.3 R9: END status 0, the outcome kind, its bytes in args encoding.
        s.put(bytes([10, 2, 0, kind]) + args(outcome)); s.close(2)
        return b''.join(s.blocks)

t = Tape()
t.grant(CLOCK)                    # ordinal 1
t.grant(K_STDOUT)                 # ordinal 2
t.grant(DIR)                      # ordinal 3
t.grant(ARGS, lb(b'a') + lb(b'bc'))   # ordinal 4
t.op(WALL_MS, 1, b'', 1_700_000_000_000)
t.op(MONO_NS, 1, b'', 5_000)
t.op(MONO_NS, 1, b'', 4_000)                 # a clock going back: delta -1000
t.op(WALL_MS, 1, b'', 1_700_000_000_250)
t.op(STDOUT, 2, b'hello', written=5)
t.op(STDOUT, 2, b'0123456789abcdefXYZ', written=19)   # 19 bytes: digest
t.op(OPEN, 3, bytes([0]) + b'missing', error=(1, 0))  # NotFound
t.op(OPEN, 3, bytes([0]) + b'data', 5)                # ordinal 5
big = bytes((i * 7 + 1) & 0xff for i in range(70_000))
t.op(READ, 5, struct.pack('<Q', 70_000), 70_000, payload=big)
t.op(READ, 5, struct.pack('<Q', 10), 0, payload=b'')
t.op(CLOSE, 5, b'')
t.op(SLEEP_UNTIL, 1, struct.pack('<Q', 9_000))
t.op(RENAME, 3, b'a\0b')
t.op(MKDTEMP, 3, b't.', 6, payload=b'abc123')         # ordinal 6
t.op(SYNC_DIR, 6, b'')
# The scripted run fails with Fail("no data") then Overflow (core.hirsel:125
# order: Overflow 0 .. Fail 4 .. Capacity 6, Tape 7); sites are not recorded.
FAILED = 1
(HERE / 'hand-tape.bin').write_bytes(t.end(FAILED, vi(2) + bytes([4]) + lb(b'no data') + bytes([0])))
print('xxh3.bin rows', len(rows) // 16, 'hand-tape.bin bytes', sum(map(len, t.blocks)), 'blocks', len(t.blocks))

# Open at 0, GRANT at 1. WAIT at 2 has gap 2 - 1 = 1.
# WAKE at 2 follows WAIT without an event, so its gap is 2 - 2 = 0.
# WAIT is the first task-carrying frame, so it sets T and names task 0.
g = Tape()
g.grant(CLOCK, gap=1)
g.put(bytes([0x1b, 1, 0]))        # WAIT gap 1, task 0
g.task = 0
g.put(bytes([4, 0]))              # WAKE gap 0, previous task 0
g.op(SLEEP_UNTIL, 1, struct.pack('<Q', 9000), value=17)
(HERE / 'grant-gap-tape.bin').write_bytes(g.end(FAILED, vi(2) + bytes([4]) + lb(b'no data') + bytes([0])))

# The two-clock model places GRANT at open, with a delta of zero.
TAPE_ID = bytes(range(16))
model_header = (struct.pack('<HB', 6, 2) + TAPE_ID + lb(b'pkg/charge#0001')
                + bytes(range(32)) + struct.pack('<IQQ', 1, 1024, 0) + b'\0'
                + lb(b'arg-7') + vi(0))
first_clock = bytes([0x13, 0, MONO_NS, 1, 0, 0]) + vi(zz(1_000_000_123))
second_clock = bytes([0x13, 0, MONO_NS, 1, 0, 0]) + vi(zz(5_000))
model = b''.join([
    block(0, 0, model_header),
    block(1, 0, bytes([7, 0, CLOCK]) + first_clock),
    block(2, 0, second_clock),
    block(3, 2, bytes([10, 2, 0, 0]) + lb(struct.pack('<Q', 5_000))),
])
(HERE / 'model-f4-tape.bin').write_bytes(model)


# A real Run snapshot can contain repeated rules. Preserve their original order.
TAPE_ID = bytes(range(16, 32))
loopback = 0x7f000001
listen_rule = struct.pack('<BBQQBHH', 1, 0, 0, loopback, 32, 0, 65535)
connect_rule = struct.pack('<BBQQBHH', 0, 1, 0, 1, 128, 1, 65535)
grants = bytes([NET]) + vi(3) + listen_rule + connect_rule + listen_rule
header = (struct.pack('<HB', 6, 1) + TAPE_ID + lb(b'app@1/main/run') + BUILD
          + struct.pack('<IQQ', 0, 1000, 0) + bytes([0]) + lb(b'arg') + lb(grants))
net = Tape()
net.grant(NET)
net.grant(CLOCK)
addr = lambda port: struct.pack('<BQQH', 0, 0, loopback, port)
net.op(LISTEN, 1, addr(0) + struct.pack('<H', 8), 3, payload=addr(49217))
deadline = vi(2) + struct.pack('<Q', 5000)
net.put(bytes([1, ACCEPT]) + vi(3) + args(deadline))
net.put(bytes([2, 0, 0]) + vi(4) + lb(addr(49218)))
net.op(RECV_BOUNDED, 4, struct.pack('<Q', 70000) + deadline, 70000, payload=big)
net.op(SEND_PAIR, 4, deadline + lb(b'abc') + lb(b'def'), value=4)
net.op(RECV_BOUNDED, 4, struct.pack('<Q', 1) + deadline, error=(5, 0))
(HERE / 'net-tape.bin').write_bytes(net.end(0, struct.pack('<I', 0)))
