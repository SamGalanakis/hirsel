"""Prepare OpenH264's own decoder vectors and C reference outputs.

Usage: prepare.py WORK REFERENCE_DIR OPENH264_TARBALL

REFERENCE_DIR is the OPENH264_REFERENCE_DIR output of build.sh: the same
libopenh264.a objects the archive links, called directly from reference.c.
"""

from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "tools/toolchain-dist"))
import hcfg
import tarfile

TARBALL_SHA256 = "5178775c8b440073e70920e97d195efff721c0e56d0a78cc82c919e95d54924d"
ROOT = "openh264-2.7.0"
# Coded size the law opens each decoder with: 1080p with macroblock padding.
MAX_WIDTH, MAX_HEIGHT = 1920, 1088
# NAL kinds the wrapper accepts: non-IDR and IDR slices, SEI, SPS, PPS, AUD,
# end of sequence, end of stream and filler.
ACCEPTED_KINDS = {1, 5, 6, 7, 8, 9, 10, 11, 12}
# (input, width, height, fps, bitrate) for the encode arms.
ENCODE = (
    ("CiscoVT2people_160x96_6fps.yuv", 160, 96, 6, 120000),
    ("CiscoVT2people_320x192_12fps.yuv", 320, 192, 12, 300000),
)


def nal_units(stream):
    """Split an Annex B stream at each start code, keeping the start code."""
    starts = [m.start() for m in re.finditer(b"\x00\x00\x01", stream)]
    units = []
    for i, start in enumerate(starts):
        begin = start - 1 if start > 0 and stream[start - 1] == 0 else start
        end = starts[i + 1] if i + 1 < len(starts) else len(stream)
        if i + 1 < len(starts) and stream[end - 1] == 0:
            end -= 1
        units.append(stream[begin:end])
    if starts and any(stream[:starts[0] - 1]):
        raise RuntimeError("bytes before the first start code")
    return units


class Bits:
    def __init__(self, rbsp):
        self.data = rbsp
        self.at = 0

    def bit(self):
        byte = self.data[self.at >> 3]
        self.at += 1
        return (byte >> (7 - ((self.at - 1) & 7))) & 1

    def u(self, n):
        value = 0
        for _ in range(n):
            value = value << 1 | self.bit()
        return value

    def ue(self):
        zeros = 0
        while self.bit() == 0:
            zeros += 1
        return (1 << zeros) - 1 + self.u(zeros)

    def se(self):
        k = self.ue()
        return (k + 1) // 2 if k & 1 else -(k // 2)


def rbsp(payload):
    return re.sub(b"\x00\x00\x03", b"\x00\x00", payload)


def sps_format(unit):
    """H.264 7.3.2.1.1: coded size, frame_mbs_only, chroma format and depth."""
    payload = unit[unit.index(b"\x00\x00\x01") + 4:]
    r = Bits(rbsp(payload))
    profile = r.u(8)
    r.u(16)
    r.ue()
    chroma, depth, chroma_depth = 1, 0, 0
    if profile in (100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135):
        chroma = r.ue()
        if chroma == 3:
            r.bit()
        depth = r.ue()
        chroma_depth = r.ue()
        r.bit()
        if r.bit():
            for i in range(8 if chroma != 3 else 12):
                if r.bit():
                    last, following = 8, 8
                    for _ in range(16 if i < 6 else 64):
                        if following:
                            following = (last + r.se() + 256) % 256
                        last = following or last
    r.ue()
    poc_type = r.ue()
    if poc_type == 0:
        r.ue()
    elif poc_type == 1:
        r.bit()
        r.se()
        r.se()
        for _ in range(r.ue()):
            r.se()
    r.ue()
    r.bit()
    width = (r.ue() + 1) * 16
    map_units = r.ue() + 1
    frame_mbs_only = r.bit()
    height = map_units * (2 - frame_mbs_only) * 16
    return profile, width, height, frame_mbs_only == 1 and chroma == 1 and depth == 0 and chroma_depth == 0


def scope(units):
    """Why the wrapper's documented scope excludes a stream, or None."""
    for unit in units:
        kind = unit[unit.index(b"\x00\x00\x01") + 3] & 31
        if kind not in ACCEPTED_KINDS:
            return f"NAL kind {kind}"
        if kind == 7:
            _, width, height, progressive_420 = sps_format(unit)
            if not progressive_420:
                return "interlaced, non-4:2:0 or high bit depth SPS"
            if width > MAX_WIDTH or height > MAX_HEIGHT:
                return f"coded size {width}x{height}"
    return None


def run(*command):
    result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    if result.returncode:
        raise RuntimeError(f"{command}: {result.stdout}")


def planes_sha1(decoded):
    """Hash the planes of reference.c output the way decoder_test.cpp does."""
    digest = hashlib.sha1()
    at = 0
    while at < len(decoded):
        width, height = struct.unpack_from("<II", decoded, at)
        size = width * height * 3 // 2
        digest.update(decoded[at + 16:at + 16 + size])
        at += 16 + size
    return digest.hexdigest()


def decode_vector(reference, source, fixtures, index, name, upstream):
    units = nal_units((source / "res" / name).read_bytes())
    stem = fixtures / f"d-{index}"
    stem.with_suffix(".nal").write_bytes(b"".join(struct.pack("<I", len(u)) + u for u in units))
    run(reference, "decode", stem.with_suffix(".nal"), stem.with_suffix(".demo"))
    digest = planes_sha1(stem.with_suffix(".demo").read_bytes())
    agrees = "matches" if digest == upstream else f"differs from decoder_test.cpp {upstream}"
    return f"d-{index} {name}: {len(units)} NALs, C SHA-1 {digest} {agrees}"


def encode_vector(reference, source, fixtures, index, profile, arm):
    name, width, height, fps, bitrate = arm
    stem = fixtures / f"e-{index}"
    shutil.copyfile(source / "res" / name, stem.with_suffix(".yuv"))
    run(reference, profile, str(width), str(height), str(fps), str(bitrate),
        stem.with_suffix(".yuv"), stem.with_suffix(".encoded"))
    packets = stem.with_suffix(".encoded").read_bytes()
    size, = struct.unpack_from("<I", packets, 0)
    sps = next(u for u in nal_units(packets[4:4 + size]) if u[u.index(b"\x00\x00\x01") + 3] & 31 == 7)
    expected = 77 if profile == "main" else 66
    if sps_format(sps)[0] != expected:
        raise RuntimeError(f"{name} {profile}: SPS profile_idc is not {expected}")
    stem.with_suffix(".config").write_bytes(struct.pack("<IIIII", width, height, fps, bitrate, profile == "main"))
    return f"e-{index} {name} {profile}: {len(packets)} bytes"


def main():
    work, reference_dir, tarball = (Path(arg).resolve() for arg in sys.argv[1:])
    vectors = Path(__file__).resolve().parent
    codec = vectors.parent
    fixtures = work / "fixtures"
    package = work / "h264"
    shutil.rmtree(work, ignore_errors=True)
    fixtures.mkdir(parents=True)
    package.mkdir()
    # A path dependency must be a workspace member.
    hcfg.workspace(work, ["h264", "media_bits"])
    shutil.copytree(codec.parent / "media_bits", work / "media_bits")
    for filename in ("h264.hirsel", "native.hirsel", "libhirsel_h264.a"):
        shutil.copyfile(codec / filename, package / filename)
    shutil.copyfile(vectors / "tests.hirsel", package / "tests.hirsel")
    # The test runs in its own empty working directory, so its grant is absolute.
    hcfg.vector_package(codec, vectors, package, fixtures)

    with tarball.open("rb") as archive:
        if hashlib.file_digest(archive, "sha256").hexdigest() != TARBALL_SHA256:
            raise SystemExit("OpenH264 v2.7.0 source checksum mismatch")
    with tarfile.open(tarball) as archive:
        archive.extractall(work / "upstream", filter="data")
    source = work / "upstream" / ROOT

    llvm = Path(os.environ["LLVM_SYS_231_PREFIX"])
    reference = work / "reference"
    run(llvm / "bin/clang++", "--target=x86_64-unknown-linux-gnu", "-O2", "-x", "c", "-I", reference_dir,
        vectors / "reference.c", "-x", "none", reference_dir / "libopenh264.a",
        "-fuse-ld=lld", "-lpthread", "-lm", "-o", reference)

    listing = (source / "test/api/decoder_test.cpp").read_text()
    table = listing[listing.index("kFileParamArray[] = {"):]
    table = table[:table.index("};")]
    cases = re.findall(r'\{"res/([^"]+)", "([0-9a-f]{40})"\}', table)
    accepted = []
    for name, upstream in cases:
        reason = scope(nal_units((source / "res" / name).read_bytes()))
        if reason:
            print(f"excluded {name}: {reason}", flush=True)
        else:
            accepted.append((name, upstream))

    with ThreadPoolExecutor(max_workers=4) as executor:
        jobs = [executor.submit(decode_vector, reference, source, fixtures, i, name, upstream)
                for i, (name, upstream) in enumerate(accepted)]
        arms = [(profile, arm) for arm in ENCODE for profile in ("baseline", "main")]
        jobs += [executor.submit(encode_vector, reference, source, fixtures, i, profile, arm)
                 for i, (profile, arm) in enumerate(arms)]
        for job in jobs:
            print(job.result(), flush=True)
    (fixtures / "count").write_bytes(struct.pack("<QQ", len(accepted), len(arms)))
    shutil.rmtree(work / "upstream")


if __name__ == "__main__":
    main()
