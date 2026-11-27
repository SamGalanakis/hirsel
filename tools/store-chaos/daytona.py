"""Run the APFS qualification off-guest. The default weekly duration is 75 minutes."""
import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import json
from pathlib import Path
import shlex
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

from history import History, require
from recover import verify_copy

HERE = Path(__file__).resolve().parent
API = 'https://api.daytona.com'
RESERVATION = '0883f880-3318-4a0b-9d0a-c6871ea63015'


class Daytona:
    def __init__(self, log):
        self.key = (Path.home() / '.config/daytona/api_key_macos').read_text().strip()
        self.log = log

    def call(self, verb, url, data=None, mime='application/json', operation=None):
        headers = {'Authorization': 'Bearer ' + self.key, 'Content-Type': mime}
        if operation:
            headers['Idempotency-Key'] = operation
        if data is not None and not isinstance(data, bytes):
            data = json.dumps(data).encode()
        req = urllib.request.Request(url, data=data, method=verb, headers=headers)
        with urllib.request.urlopen(req, timeout=120) as r:
            return r.status, r.read()

    def json(self, *args, **kwargs):
        status, raw = self.call(*args, **kwargs)
        return status, json.loads(raw) if raw else {}

    def record(self, text):
        with self.log.open('a') as f:
            print(datetime.now(timezone.utc).isoformat(), text, file=f, flush=True)

    @contextmanager
    def sandbox(self, reservation, seconds):
        _, info = self.json('GET', API + '/reservations/' + reservation)
        require(info['status'] == 'active' and info['slots_used'] == 0, 'Mac reservation occupied or inactive')
        name = 'store-chaos-' + uuid.uuid4().hex[:12]
        body = dict(name=name, reservation=reservation, image='base-macos', persistent=False,
                    lifecycle_policy=dict(timeout_seconds=seconds, on_timeout='delete', idle_expiry_seconds=-1))
        status, sb = self.json('POST', API + '/sandboxes/macos', body, operation=name)
        url = API + '/sandboxes/' + sb['id']
        self.record(f"POST {sb['id']} HTTP {status}")
        try:
            for _ in range(75):
                _, state = self.json('GET', url)
                if state.get('status') in ('running', 'ready', 'started'):
                    break
                time.sleep(2)
            else:
                raise RuntimeError('Mac boot did not finish')
            self.proxy = 'https://proxy.bos-1.us-east.daytona.app/toolbox/' + sb['id']
            yield
        finally:
            for attempt in range(6):
                try:
                    code, _ = self.call('DELETE', url)
                    self.record(f"DELETE {sb['id']} HTTP {code}")
                    break
                except urllib.error.HTTPError as e:
                    if e.code in (404, 410):
                        break
                    if attempt == 5:
                        raise
                    time.sleep(2)
            for _ in range(60):
                try:
                    code, state = self.json('GET', url)
                    if state.get('status') == 'deleted':
                        break
                except urllib.error.HTTPError as e:
                    code = e.code
                    if code in (404, 410):
                        break
                    raise
                time.sleep(2)
            else:
                raise RuntimeError('Mac deletion unconfirmed')
            self.record(f"GET {sb['id']} HTTP {code} deleted")

    def command(self, command):
        addr = self.proxy + '/processes'
        query = '?waitMs=50000&logs=text'
        _, reply = self.json('POST', addr + query, dict(kind='exec', shellCommand=command))
        while reply['state'] != 'terminal':
            _, reply = self.json('POST', addr + '/' + reply['id'] + '/wait' + query, {})
        output = ''.join(row['data'] for row in reply.get('logs', {}).get('frames', []))
        if reply['exitCode']:
            raise RuntimeError(f"Mac command exited {reply['exitCode']}: {output[-1200:]}")
        return output

    def upload(self, path, name):
        boundary = uuid.uuid4().hex
        fields = f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="file"\r\n\r\n'
        data = fields.encode() + path.read_bytes() + f'\r\n--{boundary}--\r\n'.encode()
        self.call('POST', self.proxy + '/files/upload?' + urllib.parse.urlencode({'path': name}),
                  data, 'multipart/form-data; boundary=' + boundary)


def qualify(mac, opts, remote):
    def guest(*parts):
        return json.loads(mac.command(shlex.join(['python3', remote + '/mac_guest.py', *map(str, parts)])))

    def emit(name, status, **fields):
        with (opts.receipts / 'apfs.ndjson').open('a') as f:
            print(json.dumps(dict(law='N1', row=name, status=status, **fields)), file=f)

    started = time.monotonic()
    for cycle in range(1_000_000):
        init = guest('setup')
        require(init['rc'] == 0, str(init))
        try:
            h = History(opts.receipts / f'apfs-{cycle}.history', int(init['output'].split()[1]))
            for op in (1, 2):
                h.invoke(op)
                reply = guest('tx', op)
                h.returned(op, reply['output'])
                require(reply['rc'] == 0, str(reply))
            pressure = guest('pressure')
            require(pressure['errno'] == 28 and pressure['bytes'] > 0, 'APFS image did not fill')
            emit('bounded-image-ENOSPC', 'pass', scheduled='fill sparse image', native=pressure)
            for i, when in enumerate(('before', 'after'), 3):
                h.invoke(i)
                result = guest('cut', when, i)
                h.returned(i, result['output'])
                signals = result['native'] + init['native']
                native = [s for s in signals if s.startswith('CUT ' + when)]
                require(result['rc'] in (0, 86), str(result))
                target = opts.receipts / f'apfs-{cycle}-{when}'
                verify_copy(guest('cold')['files'], target, opts.hirsel, opts.release)
                dump = guest('dump')
                require(dump['rc'] == 0, str(dump))
                oracle = h.verify(dump['output'])
                hit = bool(native) and result['rc'] == 86 and all(
                    'FULLFSYNC ' + kind + ' 0 0' in signals for kind in ('file', 'directory'))
                emit('rename-' + when, 'pass' if hit else 'incomplete', scheduled=when,
                     native=signals, **oracle)
                require(hit, 'missing APFS rename or F_FULLFSYNC witness')
        finally:
            guest('cleanup')
        if time.monotonic() - started >= opts.minutes * 60:
            return


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('fixture', type=Path)
    p.add_argument('hirsel', type=Path)
    p.add_argument('receipts', type=Path)
    p.add_argument('--minutes', type=int, default=75, choices=(0, *range(60, 91)))
    p.add_argument('--reservation', default=RESERVATION)
    p.add_argument('--release', type=Path)
    args = p.parse_args()
    args.receipts.mkdir(parents=True, exist_ok=True)
    mac = Daytona(args.receipts / 'LEDGER.md')
    with mac.sandbox(args.reservation, 900 + args.minutes * 60):
        remote = mac.command('mktemp -d /tmp/store-chaos.XXXXXXXX').strip()
        for file, name in [(args.fixture, 'fixture'), (HERE / 'mac_guest.py', 'mac_guest.py'),
                           (HERE / 'mac_interpose.c', 'mac_interpose.c')]:
            mac.upload(file, remote + '/' + name)
        mac.command(shlex.join(['chmod', '700', remote + '/fixture']))
        qualify(mac, args, remote)


if __name__ == '__main__':
    main()
