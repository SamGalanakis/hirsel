"""JSON-lines worker, run as root inside the disposable Linux guest or private mount namespace."""
import base64
from contextlib import ExitStack
import errno
import gzip
import json
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import sys
import time

from linux_devices import Volume, run

HERE = Path(__file__).resolve().parent
FIXTURE = HERE / 'fixture'


def pack(path):
    return base64.b64encode(gzip.compress(path.read_bytes(), compresslevel=1)).decode()


class Guest:
    def __init__(self):
        self.stack = ExitStack()
        self.writer = None

    def setup(self, fs, fault):
        self.fault = fault
        target = ('dust' if fault.startswith('dust') else 'flakey' if fault.startswith('flakey')
                  else 'log-writes' if fault == 'log-writes' else 'linear')
        # dmsetup targets lists loaded targets only. Request this one before checking.
        subprocess.run(['modprobe', 'dm_' + target.replace('-', '_')], capture_output=True)
        if target not in run('dmsetup', 'targets').decode().split():
            return dict(reason=f'unavailable dm-{target}')
        if fault == 'ring':
            version = tuple(map(int, platform.release().split('-')[0].split('.')[:2]))
            if version < (6, 13):
                return dict(reason=f'{platform.release()} selects epoll')
            if not shutil.which('bpftrace'):
                return dict(reason='bpftrace required for native ring CQEs')
        self.vol = Volume(fs, fault, self.stack)
        self.trace = self.vol.root / 'syscalls'
        init = run(FIXTURE, 'init', cwd=self.vol.store).decode()
        env = dict(os.environ, HIRSEL_NET_REACTOR='auto' if fault == 'ring' else 'epoll',
                   HIRSEL_UNTIED='2')
        cmd = [str(FIXTURE), 'serve']
        if fault.startswith('space-'):
            run('cc', '-shared', '-O2', '-fPIC', HERE / 'fill.c', '-ldl', '-o', self.vol.root / 'fill.so')
            site = 'journal-pwrite' if fault == 'space-journal' and fs == 'xfs' else fault[6:]
            env.update(LD_PRELOAD=str(self.vol.root / 'fill.so'), CHAOS_FILL_SITE=site,
                       CHAOS_FILL_PATH=str(self.vol.mount / 'fill'), CHAOS_ARM=str(self.vol.root / 'armed'))
        if fault != 'ring':
            cmd = ['strace', '-D', '-f', '-yy', '-s', '128', '-o', self.trace, '-e',
                   'trace=openat,openat2,close,pwrite64,pwritev,pwritev2,write,fsync,fdatasync,renameat,renameat2',
                   *cmd]
        else:
            from ring import Ring
            self.ring = Ring(self.vol.root, self.stack)
        self.writer = subprocess.Popen(cmd, cwd=self.vol.store, stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=sys.stderr, env=env)
        self.stack.callback(self.stop)
        if self.writer.stdout.readline() != b'READY\n':
            raise RuntimeError('fixture failed to open')
        return dict(base=int(init.split()[1]), kernel=platform.release())

    def stop(self):
        if self.writer and self.writer.poll() is None:
            try:
                comm = Path(f'/proc/{self.writer.pid}/comm').read_text().strip()
            except FileNotFoundError:
                comm = FIXTURE.name
            if comm != FIXTURE.name:
                raise RuntimeError(f'fixture PID has unexpected comm {comm}')
            self.writer.kill()
            self.writer.wait()
        if self.writer:
            self.writer.stdin.close()
            self.writer.stdout.close()
            self.writer = None

    def tx(self, op, ambiguous=False, crash=False):
        try:
            self.writer.stdin.write(bytes([op]))
            self.writer.stdin.flush()
        except BrokenPipeError:
            return ''
        rows = []
        while True:
            line = self.writer.stdout.readline().decode()
            if not line:
                break
            rows.append(line)
            if line.startswith(('ACK ', 'CUT')):
                break
        if crash:
            self.stop()
        return '' if ambiguous else ''.join(rows)

    def arm(self):
        (self.vol.root / 'armed').touch()
        if self.fault == 'log-writes':
            self.vol.mark('ack-floor')
        if self.fault == 'flakey-drop':
            self.stop()
            self.vol.flakey('drop_writes')
            return self.vol.drop_witness()
        if self.fault == 'flakey-eio':
            self.vol.flakey('error_writes')
        if self.fault == 'damage-D6':
            # Let op 2's 10 ms END cadence flush so op 3 starts a fresh END window.
            time.sleep(0.05)
        if self.fault == 'ring':
            self.ring.start(self.writer.pid)
            # Leave one block for a natural short WRITEV when extension crosses it.
            file = self.vol.mount / 'fill'
            with file.open('wb', buffering=0) as f:
                while True:
                    try:
                        f.write(bytes(65536))
                    except OSError as exc:
                        if exc.errno != errno.ENOSPC:
                            raise
                        break
                os.ftruncate(f.fileno(), max(0, f.tell() - 4096))
        return {}

    def cold(self):
        self.stop()
        if self.fault == 'log-writes':
            return dict(cases=self.vol.finish_log())
        if self.fault.startswith('flakey'):
            self.vol.heal()
        (self.vol.mount / 'fill').unlink(missing_ok=True)
        self.vol.unmount()
        self.vol.cold_mount(self.vol.device)
        return {}

    def image(self, case=None):
        if case is not None:
            self.vol.open_replay(case)
        return {p.name: pack(p) for p in self.vol.store.iterdir() if p.is_file()}

    def dump(self):
        return run(FIXTURE, 'dump', cwd=self.vol.store).decode()

    def witness(self):
        if self.fault == 'ring':
            return self.ring.witness()
        patterns = {'space-journal': r'(?:pwritev2?|pwrite64)\(.*?/j-\d+>',
                    'space-pack': r'(?:pwrite64|write|fsync|fdatasync)\(.*?/p-\d+[^>]*>',
                    'space-manifest': r'(?:pwrite64|write|fsync|fdatasync)\(.*?/MANIFEST\.tmp>',
                    'space-rename': r'renameat2?\(.*"MANIFEST\.tmp".*"MANIFEST"',
                    'flakey-eio': r'(?:fsync|fdatasync|pwrite64|pwritev2?|write)\('}
        pattern = patterns[self.fault]
        err = 'EIO' if self.fault == 'flakey-eio' else 'ENOSPC'
        return [s for s in self.trace.read_text().splitlines()
                if re.search(pattern, s) and re.search(r'= -1 ' + err + r'\b', s)]

    def dust(self, kind):
        self.stop()
        pattern = 'p-*' if kind == 'pack' else 'j-*'
        files = [p for p in self.vol.store.glob(pattern) if p.name[2:].isdecimal()]
        target = max(files, key=lambda p: int(p.name[2:]))
        mapping = run('filefrag', '-v', '-b4096', target).decode()
        found = re.search(r'^\s*0:\s*0\.\.\s*\d+:\s*(\d+)\.\.', mapping, re.M)
        if not found:
            return []
        bad = found[1]
        self.vol.unmount()
        run('dmsetup', 'message', self.vol.name, '0', 'addbadblock', bad)
        run('dmsetup', 'message', self.vol.name, '0', 'enable')
        self.vol.cold_mount(self.vol.device)
        dd = subprocess.run(['dd', f'if={self.vol.device}', 'bs=4096', f'skip={bad}', 'count=1',
                             'iflag=direct', 'status=none', 'of=/dev/null'], capture_output=True)
        proc = subprocess.run([FIXTURE, 'dump'], cwd=self.vol.store, capture_output=True)
        if dd.returncode == 0:
            return []
        if proc.returncode != 4 or proc.stdout.strip() not in (b'OPEN Io', b'OPEN Corrupt'):
            raise ValueError('bad sector reached, Store open failed to report a typed error')
        return [dict(file=target.name, block=int(bad), device=dd.stderr.decode(), store=proc.stdout.decode())]

    def damage(self, seq, end):
        segment = self.vol.store / f'j-{seq}'
        covered = struct.unpack_from('<Q', segment.with_suffix('.end').read_bytes(), 88)[0]
        if covered >= end:
            return {}
        with segment.open('r+b', buffering=0) as f:
            f.seek(end - 1)
            old = f.read(1)
            if not old:
                raise ValueError('acked frame past file')
            f.seek(end - 1)
            f.write(bytes([old[0] ^ 1]))
            os.fsync(f.fileno())
        return dict(segment=seq, covered=covered, synced_end=end, flip=end - 1)

    def close(self):
        self.stack.close()
        return {}


def main():
    guest = Guest()
    try:
        for line in sys.stdin:
            action, *args = json.loads(line)
            try:
                if action.startswith('_') or action not in ('setup', 'tx', 'arm', 'cold', 'image', 'dump',
                                                             'witness', 'dust', 'damage', 'close'):
                    raise ValueError('unknown action')
                response = dict(ok=getattr(guest, action)(*args))
            except Exception as exc:
                response = dict(error=str(exc))
            print(json.dumps(response), flush=True)
    finally:
        guest.close()


if __name__ == '__main__':
    main()
