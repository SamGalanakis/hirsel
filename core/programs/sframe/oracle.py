#!/usr/bin/env python3
"""Independent reference for docs/spec/sframe.md, and the generator of vectors.hirsel.

Written from RFC 9605 and SFRAME-9's text over Python's hmac and cryptography's AESGCM. It
never reads Hirsel output.

    python3 oracle.py vectors RFC9605.TXT > vectors.hirsel
    python3 oracle.py seal BASE_HEX KID CTR bare|annexb LAYOUT METADATA_HEX FRAME_HEX
        LAYOUT is "at:len,at:len" or "-"; prints the sealed frame as hex.
    python3 oracle.py g2 CHROME > g2_vectors.hirsel
        Law G2's capture: libx264 frames from ffmpeg and Chrome's own frames through headless
        Chromium (capture.mjs, Playwright), sealed here, recorded on the receive side.
"""
import hashlib
import hmac
import json
import os
import pathlib
import random
import re
import subprocess
import sys
import tempfile

from cryptography.hazmat.primitives.ciphers.aead import AESGCM

START = b"\x00\x00\x00\x01"


# ---------------------------------------------------------------- RFC 9605


def extract(salt, ikm):
    return hmac.new(salt, ikm, hashlib.sha256).digest()


def expand(prk, info, n):
    return hmac.new(prk, info + b"\x01", hashlib.sha256).digest()[:n]


def keys(base, kid):
    secret = extract(b"", base)
    tail = kid.to_bytes(8, "big") + (4).to_bytes(2, "big")
    key = expand(secret, b"SFrame 1.0 Secret key " + tail, 16)
    salt = expand(secret, b"SFrame 1.0 Secret salt " + tail, 12)
    return key, salt


def ratchet(base):
    return expand(extract(b"", base), b"SFrame 1.0 Ratchet", 32)


def value_bytes(x):
    if x < 8:
        return b""
    n = (x.bit_length() + 7) // 8
    return x.to_bytes(n, "big")


def header(kid, ctr):
    k, c = value_bytes(kid), value_bytes(ctr)
    hi = kid if not k else 8 | (len(k) - 1)
    lo = ctr if not c else 8 | (len(c) - 1)
    return bytes([(hi << 4) | lo]) + k + c


def nonce(salt, ctr):
    return bytes(a ^ b for a, b in zip(salt, ctr.to_bytes(12, "big")))


def seal_rfc(base, kid, ctr, metadata, plain):
    key, salt = keys(base, kid)
    h = header(kid, ctr)
    return h + AESGCM(key).encrypt(nonce(salt, ctr), plain, h + metadata)


# ---------------------------------------------------------------- SFRAME-9


def uleb(v):
    out = bytearray()
    while True:
        b = v & 0x7F
        v >>= 7
        if v:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def enc_layout(layout):
    out = uleb(len(layout))
    end = 0
    for at, n in layout:
        out += uleb(at - end) + uleb(n)
        end = at + n
    return out


def enc_starts(starts):
    """SFRAME-9 v4: the NAL count, then each start code's gap from the end of the previous one
    (from 0 for the first), in working-form offsets. Starts 0 and 8 give 02 00 04."""
    out, end = uleb(len(starts)), 0
    for p in starts:
        out += uleb(p - end)
        end = p + 4
    return out


assert enc_starts([0, 8]) == bytes([2, 0, 4]) and enc_starts([]) == b"\x00"


def unescape(body):
    out, zeros = bytearray(), 0
    for x in body:
        if zeros >= 2 and x == 3:
            zeros = 0
            continue
        out.append(x)
        zeros = zeros + 1 if x == 0 else 0
    return bytes(out)


def interior(body):
    out, zeros = bytearray(), 0
    for x in body:
        if zeros >= 2 and x <= 3:
            out.append(3)
            zeros = 0
        out.append(x)
        zeros = zeros + 1 if x == 0 else 0
    return out, zeros


def wire_escape(body):
    out, _ = interior(body)
    if out.rstrip(b"\x03").endswith(b"\x00"):
        out.append(3)
    return bytes(out)


def codec_escape(body):
    out, zeros = interior(body)
    if zeros >= 2:
        out.append(3)
    return bytes(out)


def nal_bodies(frame):
    """The NAL bodies of an input frame that meets SFRAME-9's input rules."""
    assert frame.startswith(START)
    parts = frame.split(START)[1:]
    for p in parts:
        assert b"\x00\x00\x01" not in p and not p.endswith(b"\x00")
        assert codec_escape(unescape(p)) == p
    return parts


def seal_ranged(base, kid, ctr, annexb, layout, metadata, frame):
    if annexb:
        bodies = [unescape(p) for p in nal_bodies(frame)]
        work, starts = b"", []
        for b in bodies:
            starts.append(len(work))
            work += START + b
    else:
        work, starts = frame, []
    pos, clear, secret = 0, b"", b""
    for at, n in layout:
        secret += work[pos:at]
        clear += work[at : at + n]
        pos = at + n
    secret += work[pos:]
    h, e = header(kid, ctr), enc_layout(layout)
    key, salt = keys(base, kid)
    sealed = AESGCM(key).encrypt(nonce(salt, ctr), secret, h + e + enc_starts(starts) + clear + metadata)
    ct, tag = sealed[:-16], sealed[-16:]
    body, pos, src = bytearray(work), 0, 0
    for at, n in layout:
        body[pos:at] = ct[src : src + at - pos]
        src += at - pos
        pos = at + n
    body[pos:] = ct[src:]
    body += tag + h + e + (len(h) + len(e)).to_bytes(2, "big")
    if not annexb:
        return bytes(body)
    out = b""
    for i, s in enumerate(starts):
        end = starts[i + 1] if i + 1 < len(starts) else len(body)
        out += START + wire_escape(bytes(body[s + 4 : end]))
    return out


# ---------------------------------------------------------------- libwebrtc's fake SPSs


class BitWriter:
    def __init__(self):
        self.bits = []

    def put(self, v, n):
        self.bits += [(v >> (n - 1 - i)) & 1 for i in range(n)]

    def ue(self, v):
        x = v + 1
        n = x.bit_length()
        self.put(0, n - 1)
        self.put(x, n)

    def bytes(self):
        bits = self.bits + [0] * (-len(self.bits) % 8)
        return bytes(int("".join(map(str, bits[i : i + 8])), 2) for i in range(0, len(bits), 8))


def fake_sps(mode):
    """GenerateFakeSps in libwebrtc's common_video/h264/sps_vui_rewriter_unittest.cc, 640x480,
    with no colour description: mode is "optimal" (kVuiNoFrameBuffering), "novui",
    "norestriction" or "suboptimal" (kVuiFrameBuffering). The generator writes no
    rbsp_trailing_bits; these add them, as an encoder does, so no NAL ends in 00. Answers
    the NAL with header 0x67."""
    w = BitWriter()
    w.put(0, 8)
    w.put(0, 8)
    w.put(3, 8)
    for v in (0, 0, 0, 0, 1):
        w.ue(v)
    w.put(0, 1)
    w.ue((640 + 15) // 16 - 1)
    w.ue(((480 + 15) // 16 - 1) // 2)
    w.put(0, 1)
    w.put(0, 1)
    w.put(0, 1)
    w.put(1, 1)
    for v in (0, 0, 0, 0):
        w.ue(v)
    vui = mode != "novui"
    w.put(int(vui), 1)
    if vui:
        w.put(0, 2)
        w.put(0, 1)
        w.put(0, 5)
        restricted = mode != "norestriction"
        w.put(int(restricted), 1)
        if restricted:
            w.put(1, 1)
            for v in (2, 1, 16, 16):
                w.ue(v)
            reorder, dec = (0, 1) if mode == "optimal" else (3, 3)
            w.ue(reorder)
            w.ue(dec)
    w.put(1, 1)
    return b"\x67" + interior(w.bytes())[0]


# ---------------------------------------------------------------- vectors


def h(b):
    return b.hex()


def header_cases(rfc_text):
    text = rfc_text.split("C.1.  Header Encoding/Decoding")[-1].split("C.2.")[0]
    found = re.findall(r"kid: 0x([0-9a-f]+)\s+ctr: 0x([0-9a-f]+)\s+header: ([0-9a-f]+(?:\n {11}[0-9a-f]+)*)", text)
    return [(kid, ctr, re.sub(r"\s", "", hd)) for kid, ctr, hd in found]


def random_layout(rng, n, must):
    """A canonical layout over n bytes containing every range of `must`."""
    marks = [0] * (n + 1)
    for at, k in must:
        for i in range(at, at + k):
            marks[i] = 1
    for _ in range(rng.randrange(0, 4)):
        if n == 0:
            break
        at = rng.randrange(0, n)
        for i in range(at, min(n, at + rng.randrange(1, 12))):
            marks[i] = 1
    layout, i = [], 0
    while i < n:
        if marks[i]:
            j = i
            while j < n and marks[j]:
                j += 1
            layout.append((i, j - i))
            i = j
        else:
            i += 1
    return layout


def zeroish(rng, n):
    return bytes(rng.choice([0, 0, 0, 1, 2, 3, rng.randrange(256)]) for _ in range(n))


EDGES = [0, 7, 8, 255, 256, 2**56 - 1, 2**56]


def ranged_cases():
    rng = random.Random(9605)
    cases = []
    for i in range(32):
        annexb = i % 2 == 1
        kid = EDGES[i % len(EDGES)]
        ctr = [0, 7, 8, 255, 256, 2**56, 2**64 - 2][(i // 2) % 7]
        base = bytes(rng.randrange(256) for _ in range(16))
        metadata = bytes(rng.randrange(256) for _ in range(rng.randrange(0, 8)))
        if annexb:
            raw = []
            for _ in range(rng.randrange(1, 5)):
                b = zeroish(rng, rng.randrange(1, 60))
                if b.endswith(b"\x00"):
                    b += b"\x41"
                raw.append(b)
            frame = b"".join(START + codec_escape(b) for b in raw)
            starts, p = [], 0
            for b in raw:
                starts.append((p, 4))
                p += 4 + len(b)
            layout = random_layout(rng, p, starts)
        else:
            frame = zeroish(rng, rng.randrange(0, 200))
            layout = random_layout(rng, len(frame), [])
        wire = seal_ranged(base, kid, ctr, annexb, layout, metadata, frame)
        cases.append((annexb, kid, ctr, base, layout, metadata, frame, wire))
    return cases


def emit_vectors(rfc_path):
    rfc = open(rfc_path, encoding="utf-8").read()
    print("// Generated by python3 oracle.py vectors rfc9605.txt. Header cases are RFC 9605")
    print("// Appendix C.1; ranged cases are oracle.py's SFRAME-9 seal over Python hmac and AESGCM.")
    print("type HeaderCase {\n    kid: u64,\n    ctr: u64,\n    header: String\n}\n")
    print("fn header_cases() -> List[HeaderCase] {")
    print("    let mut v: List[HeaderCase] = list.new()")
    cases = header_cases(rfc)
    assert len(cases) == 289, len(cases)
    for kid, ctr, hd in cases:
        assert header(int(kid, 16), int(ctr, 16)).hex() == hd
        print(f'    list.push(mut v, HeaderCase(kid: 0x{kid}, ctr: 0x{ctr}, header: "{hd}"))')
    print("    take v\n}\n")
    print("type RangedCase {\n    annexb: bool,\n    kid: u64,\n    ctr: u64,\n    base: String,")
    print("    layout: ranges.Layout,\n    metadata: String,\n    frame: String,\n    wire: String\n}\n")
    print("fn ranged_cases() -> List[RangedCase] {")
    print("    let mut v: List[RangedCase] = list.new()")
    for annexb, kid, ctr, base, layout, metadata, frame, wire in ranged_cases():
        print("    {")
        print("        let mut rs: List[ranges.Range] = list.new()")
        for at, n in layout:
            print(f"        list.push(mut rs, ranges.Range(at: {at}, len: {n}))")
        print(
            f"        list.push(mut v, RangedCase(annexb: {'true' if annexb else 'false'}, kid: {kid}, ctr: {ctr}, "
            f'base: "{h(base)}", layout: ranges.Layout(ranges: take rs), metadata: "{h(metadata)}", '
            f'frame: "{h(frame)}", wire: "{h(wire)}"))'
        )
        print("    }")
    print("    take v\n}\n")
    for mode in ("optimal", "novui", "norestriction", "suboptimal"):
        print(f"fn fake_sps_{mode}() -> Bytes {{\n    unhex(\"{h(fake_sps(mode))}\")\n}}\n")


# ---------------------------------------------------------------- law G2


def split_annexb(frame):
    """Bodies of a raw Annex B frame at its 3- and 4-byte start codes, trailing zeros removed."""
    marks = [m.start() for m in re.finditer(b"\x00\x00\x01", frame)]
    bodies = []
    for i, m in enumerate(marks):
        end = marks[i + 1] if i + 1 < len(marks) else len(frame)
        body = frame[m + 3 : end].rstrip(b"\x00")
        if body:
            bodies.append(body)
    return bodies


def four_byte(frame):
    """The frame with 4-byte start codes and no trailing zeros; parameter sets unchanged."""
    return b"".join(START + b for b in split_annexb(frame))


class BitReader:
    def __init__(self, b):
        self.b, self.at = b, 0

    def bit(self):
        v = (self.b[self.at // 8] >> (7 - self.at % 8)) & 1
        self.at += 1
        return v

    def ue(self):
        zeros = 0
        while self.bit() == 0:
            zeros += 1
        v = 0
        for _ in range(zeros):
            v = (v << 1) | self.bit()
        return (1 << zeros) - 1 + v


def h264_layout(frame):
    """SFRAME-10's H.264 rule from H.264 §7.3.3: start codes, SPS and PPS, and each slice's
    NAL header through pic_parameter_set_id clear; other NALs keep their header byte."""
    spans, work = [], 0
    for body in nal_bodies(frame):
        raw = unescape(body)
        kind = raw[0] & 31
        if kind in (7, 8):
            keep = len(raw)
        elif kind in (1, 5):
            r = BitReader(raw[1:])
            for _ in range(3):
                r.ue()
            keep = 1 + (r.at + 7) // 8
        else:
            keep = 1
        n = 4 + keep
        if spans and spans[-1][0] + spans[-1][1] == work:
            spans[-1] = (spans[-1][0], spans[-1][1] + n)
        else:
            spans.append((work, n))
        work += 4 + len(raw)
    return spans


def capture(chrome, items):
    here = pathlib.Path(__file__).resolve().parent
    with tempfile.TemporaryDirectory() as tmp:
        src, out = os.path.join(tmp, "in.json"), os.path.join(tmp, "out.json")
        json.dump(items, open(src, "w"))
        subprocess.run(["node", str(here / "capture.mjs"), chrome, src, out], check=True, timeout=300)
        log = json.load(open(out))
    sent = {s["ts"]: s for s in log["sent"]}
    by_id = {}
    for r in log["recv"]:
        s = sent.get(r["ts"])
        if s and s["id"] is not None:
            by_id[s["id"]] = bytes.fromhex(r["hex"])
    own = [(s, r) for r in log["recv"] for s in [sent.get(r["ts"])] if s and s["id"] is None]
    if os.environ.get("G2_KEEP"):
        json.dump(log, open(os.environ["G2_KEEP"] + "-" + str(len(items)) + ".json", "w"))
    missing = [i["id"] for i in items if i["id"] not in by_id]
    assert not missing, f"not received: {missing}"
    return log["chromium"], by_id, own


def libx264_frames():
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "x.h264")
        subprocess.run(
            ["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=160x120:rate=30",
             "-frames:v", "8", "-pix_fmt", "yuv420p", "-c:v", "libx264", "-profile:v", "high", "-bf", "2",
             "-g", "8", "-x264-params", "aud=1", "-bsf:v", "h264_mp4toannexb", "-f", "h264", path],
            check=True,
        )
        data = open(path, "rb").read()
    cuts = [m.start() for m in re.finditer(b"\x00\x00\x00\x01\x09", data)]
    return [data[a:b] for a, b in zip(cuts, cuts[1:] + [len(data)])]


def is_key(frame):
    return any(b[0] & 31 == 5 for b in split_annexb(frame))


def g2_cases(chrome):
    base = bytes(range(0x40, 0x50))
    raw = libx264_frames()
    assert is_key(raw[0]) and not any(is_key(f) for f in raw[1:])
    version, first, own = capture(chrome, [{"id": f"raw{i}", "key": is_key(f), "hex": f.hex()} for i, f in enumerate(raw)])
    for s, r in own:
        assert s["hex"] == r["hex"], "Chrome changed one of its own frames"
    k = next(i for i, (s, _) in enumerate(own) if s["key"])
    chrome_frames = [bytes.fromhex(s["hex"]) for s, _ in own[k : k + 3]]
    plains = [("chrome", four_byte(f)) for f in chrome_frames]
    plains += [("normalized", four_byte(first[f"raw{i}"])) for i in range(len(raw))]
    plains += [("raw", four_byte(raw[0]))]
    items, cases = [], []
    for ctr, (kind, plain) in enumerate(plains):
        sealed = seal_ranged(base, 1, ctr, True, h264_layout(plain), b"", plain)
        items.append({"id": f"s{ctr}", "key": is_key(plain), "hex": sealed.hex()})
        cases.append((kind, ctr, plain, sealed))
    version2, second, _ = capture(chrome, items)
    assert version == version2
    return version, base, raw, first, [(k, c, p, s, second[f"s{c}"]) for k, c, p, s in cases]


def emit_g2(chrome):
    version, base, raw, first, cases = g2_cases(chrome)
    print(f"// Generated by python3 oracle.py g2 CHROME: law G2's capture from headless Chromium {version}")
    print("// (capture.mjs). Each case is sealed by oracle.py with kid 1 and its ctr over g2_base();")
    print("// `received` is what Chromium's receive-side RTCRtpScriptTransform got. H.264 only: the")
    print("// Chromium build has no H.265 encoder, so no H.265 frame could be sent.")
    print("type G2Case {\n    kind: String,\n    ctr: u64,\n    plain: String,\n    sent: String,\n    received: String\n}\n")
    print(f'fn g2_base() -> Bytes {{\n    unhex("{h(base)}")\n}}\n')
    print("fn g2_cases() -> List[G2Case] {")
    print("    let mut v: List[G2Case] = list.new()")
    for kind, ctr, plain, sent, received in cases:
        print(f'    list.push(mut v, G2Case(kind: "{kind}", ctr: {ctr}, plain: "{h(plain)}", sent: "{h(sent)}", received: "{h(received)}"))')
    print("    take v\n}\n")
    print("// libx264's keyframe as ffmpeg wrote it, and as Chromium's receiver got it unsealed.")
    print(f'fn g2_libx264_key() -> Bytes {{\n    unhex("{h(raw[0])}")\n}}\n')
    print(f'fn g2_chrome_rewrite() -> Bytes {{\n    unhex("{h(first["raw0"])}")\n}}')


def main():
    if sys.argv[1] == "vectors":
        emit_vectors(sys.argv[2])
    elif sys.argv[1] == "seal":
        base, kid, ctr, mode, layout, metadata, frame = sys.argv[2:9]
        spans = [] if layout == "-" else [tuple(int(x) for x in r.split(":")) for r in layout.split(",")]
        out = seal_ranged(
            bytes.fromhex(base), int(kid, 0), int(ctr, 0), mode == "annexb", spans,
            bytes.fromhex(metadata), bytes.fromhex(frame),
        )
        print(out.hex())
    elif sys.argv[1] == "g2":
        emit_g2(sys.argv[2])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
