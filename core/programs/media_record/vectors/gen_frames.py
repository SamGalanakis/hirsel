#!/usr/bin/env python3
"""Encode the media_record fixtures with FFmpeg and write frames.hirsel.

Two seconds of a 64x48 test pattern at 30 fps (key frames at 0 and 30) in
VP8, VP9, AV1 and H.264, and two seconds of a 440 Hz tone as 20 ms Opus
packets. Each frame is kept in the form rtp_jitter delivers it: VP8 and VP9
frames as encoded, AV1 OBUs with size fields and no temporal delimiter,
H.264 as 4-byte length-prefixed SPS, PPS and slice NAL units (no AUD or
SEI), Opus one packet per frame. Key flags come from each bitstream's own
header. Run from anywhere: python3 gen_frames.py (FFmpeg 6.1.1 was used).
"""
import base64
import os
import struct
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, '..', 'frames.hirsel')
SIZE = '64x48'
VIDEO = ['-f', 'lavfi', '-i', f'testsrc2=size={SIZE}:rate=30:duration=2', '-pix_fmt', 'yuv420p']


def ffmpeg(args, out):
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', *args, out], check=True)


def ivf(path):
    data = open(path, 'rb').read()
    assert data[:4] == b'DKIF'
    header = struct.unpack('<H', data[6:8])[0]
    at = header
    frames = []
    while at < len(data):
        size = struct.unpack('<I', data[at:at + 4])[0]
        frames.append(data[at + 12:at + 12 + size])
        at += 12 + size
    return frames


def leb128(data, at):
    value = 0
    for i in range(8):
        byte = data[at]
        at += 1
        value |= (byte & 127) << (7 * i)
        if byte < 128:
            return value, at
    raise ValueError('leb128')


def put_leb128(value):
    out = bytearray()
    while value >= 128:
        out.append((value & 127) | 128)
        value >>= 7
    out.append(value)
    return bytes(out)


def av1_unit(tu):
    out = bytearray()
    key = False
    at = 0
    while at < len(tu):
        header = tu[at]
        kind = (header >> 3) & 15
        ext = 1 if header & 4 else 0
        assert header & 2, 'IVF AV1 OBUs carry sizes'
        size, body = leb128(tu, at + 1 + ext)
        if kind != 2:
            out += tu[at:at + 1 + ext] + put_leb128(size) + tu[body:body + size]
            key = key or kind == 1
        at = body + size
    return bytes(out), key


def annexb_units(data):
    units = []
    i = 0
    starts = []
    while True:
        j = data.find(b'\x00\x00\x01', i)
        if j < 0:
            break
        starts.append(j + 3)
        i = j + 3
    for k, s in enumerate(starts):
        end = starts[k + 1] - 3 if k + 1 < len(starts) else len(data)
        unit = data[s:end]
        while unit.endswith(b'\x00'):
            unit = unit[:-1]
        units.append(unit)
    return units


def h264_frames(path):
    frames = []
    current = None
    for unit in annexb_units(open(path, 'rb').read()):
        kind = unit[0] & 31
        if kind == 9:
            if current is not None:
                frames.append(current)
            current = [bytearray(), False]
            continue
        if kind in (6,):
            continue
        current[0] += struct.pack('>I', len(unit)) + unit
        current[1] = current[1] or kind == 5
    frames.append(current)
    return [(bytes(f), k) for f, k in frames]


def ogg_packets(path):
    data = open(path, 'rb').read()
    at = 0
    packets = []
    partial = b''
    while at < len(data):
        assert data[at:at + 4] == b'OggS'
        count = data[at + 26]
        laces = data[at + 27:at + 27 + count]
        body = at + 27 + count
        for lace in laces:
            partial += data[body:body + lace]
            body += lace
            if lace < 255:
                packets.append(partial)
                partial = b''
        at = body
    return packets[2:]


def main():
    tmp = tempfile.mkdtemp(prefix='mr-frames-')
    codecs = {}
    ffmpeg([*VIDEO, '-c:v', 'libvpx', '-b:v', '60k', '-g', '30', '-keyint_min', '30', '-auto-alt-ref', '0', '-lag-in-frames', '0', '-f', 'ivf'], os.path.join(tmp, 'vp8.ivf'))
    codecs['vp8'] = [(f, (f[0] & 1) == 0) for f in ivf(os.path.join(tmp, 'vp8.ivf'))]
    ffmpeg([*VIDEO, '-c:v', 'libvpx-vp9', '-b:v', '60k', '-g', '30', '-keyint_min', '30', '-auto-alt-ref', '0', '-lag-in-frames', '0', '-row-mt', '0', '-f', 'ivf'], os.path.join(tmp, 'vp9.ivf'))
    codecs['vp9'] = [(f, (f[0] & 0x04) == 0 and (f[0] & 0x08) == 0) for f in ivf(os.path.join(tmp, 'vp9.ivf'))]
    ffmpeg([*VIDEO, '-c:v', 'libaom-av1', '-b:v', '60k', '-g', '30', '-keyint_min', '30', '-cpu-used', '8', '-lag-in-frames', '0', '-usage', 'realtime', '-f', 'ivf'], os.path.join(tmp, 'av1.ivf'))
    codecs['av1'] = [av1_unit(tu) for tu in ivf(os.path.join(tmp, 'av1.ivf'))]
    ffmpeg([*VIDEO, '-c:v', 'libx264', '-profile:v', 'baseline', '-b:v', '60k', '-g', '30', '-keyint_min', '30', '-sc_threshold', '0', '-bf', '0', '-threads', '1', '-x264-params', 'aud=1', '-f', 'h264'], os.path.join(tmp, 'h264.264'))
    codecs['h264'] = h264_frames(os.path.join(tmp, 'h264.264'))
    ffmpeg(['-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000:duration=2', '-c:a', 'libopus', '-b:a', '16k', '-frame_duration', '20', '-application', 'voip', '-ac', '1', '-f', 'ogg'], os.path.join(tmp, 'opus.ogg'))
    codecs['opus'] = [(p, True) for p in ogg_packets(os.path.join(tmp, 'opus.ogg'))]
    lines = [
        '// Generated by vectors/gen_frames.py from FFmpeg 6.1.1 (libvpx, libaom,',
        '// libx264, libopus): two seconds of 64x48 testsrc2 at 30 fps, key frames',
        '// at 0 and 30, and 2 s of a 440 Hz tone as 20 ms Opus packets. Frames are',
        "// in rtp_jitter's delivered form; each key flag is read from the frame's",
        '// own bitstream header by the generator.',
    ]
    for name, frames in codecs.items():
        keys = ''.join('1' if k else '0' for _, k in frames)
        print(name, len(frames), 'frames', sum(len(f) for f, _ in frames), 'bytes', 'keys at', [i for i, (_, k) in enumerate(frames) if k][:4], file=sys.stderr)
        if name != 'vp8':
            lines.append('')
        lines.append(f'fn {name}_frames() -> List[Fixture] {{')
        lines.append(f'    let keys = b"{keys}"')
        lines.append('    let mut xs: List[Fixture] = list.new()')
        for f, _ in frames:
            lines.append(f'    add(mut xs, keys, b"{base64.b64encode(f).decode()}")')
        lines.append('    xs')
        lines.append('}')
    open(OUT, 'w').write('\n'.join(lines) + '\n')


if __name__ == '__main__':
    main()
