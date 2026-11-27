"""Signed clean controls, dual-mode MinIO relay faults, and product Bucket Store histories (Law N2)."""
import argparse
import http.client
import json
import os
from pathlib import Path
import ssl
import subprocess
import sys
import tempfile
from urllib.parse import urlsplit
import uuid

import boto3
from botocore.config import Config
from botocore.exceptions import BotoCoreError, ClientError

from history import History
from relay import running

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[1] / 'bench/objstore'))
from object_live import cleanup

DATA = b'0123456789abcdef' * 4096


def require(ok, reason):
    if not ok:
        raise ValueError(reason)


def client(endpoint, ca, region, read_timeout=12):
    cfg = Config(signature_version='s3v4', s3={'addressing_style': 'path'},
                 connect_timeout=8, read_timeout=read_timeout, retries={'total_max_attempts': 1})
    return boto3.client('s3', endpoint_url=endpoint, verify=ca, region_name=region, config=cfg)


def put(s3, obj, body, condition, value):
    def before_sign(request, **_):
        request.headers[condition] = value
    event = 'before-sign.s3.PutObject'
    s3.meta.events.register(event, before_sign)
    try:
        return s3.put_object(**obj, Body=body)['ETag']
    finally:
        s3.meta.events.unregister(event, before_sign)


def expect_error(code, fn):
    try:
        fn()
    except ClientError as e:
        status = e.response['ResponseMetadata']['HTTPStatusCode']
        require(status == code, f'expected HTTP {code}, got {status}')
        return e.response.get('Error', {}).get('Code')
    raise ValueError(f'expected HTTP {code}')


def read(s3, obj, **kwargs):
    with s3.get_object(**obj, **kwargs)['Body'] as stream:
        return stream.read()


def upload(s3, obj):
    ident = s3.create_multipart_upload(**obj)['UploadId']
    tag = s3.upload_part(**obj, UploadId=ident, PartNumber=1, Body=DATA)['ETag']
    return dict(**obj, UploadId=ident, MultipartUpload={'Parts': [{'PartNumber': 1, 'ETag': tag}]})


def controls(s3, obj):
    tag = put(s3, obj, b'genesis', 'If-None-Match', '*')
    expect_error(412, lambda: put(s3, obj, b'duplicate', 'If-None-Match', '*'))
    new = put(s3, obj, DATA, 'If-Match', tag)
    expect_error(412, lambda: put(s3, obj, b'stale', 'If-Match', tag))
    require(read(s3, obj) == DATA and tag != new, 'conditional readback')
    part = dict(obj, Key=obj['Key'] + '-mpu')
    s3.complete_multipart_upload(**upload(s3, part))
    require(read(s3, part) == DATA, 'multipart readback')
    pending = upload(s3, part)
    s3.abort_multipart_upload(**part, UploadId=pending['UploadId'])
    expect_error(404, lambda: s3.list_parts(**part, UploadId=pending['UploadId']))
    return new


def transport(relay, ca, signed, fault):
    parsed = urlsplit(signed)
    ctx = ssl.create_default_context(cafile=ca)
    ctx.maximum_version = ssl.TLSVersion.TLSv1_2
    conn = http.client.HTTPSConnection('127.0.0.1', relay.server_address[1], timeout=0.8, context=ctx)
    try:
        conn.request('GET', parsed.path + '?' + parsed.query, headers={'Host': parsed.netloc})
        response = conn.getresponse()
        require(response.status == 200, 'TCP control origin refused signed GET')
        payload = response.read()
    except (OSError, http.client.HTTPException) as exc:
        require(bool(fault), 'TCP control failed')
        return type(exc).__name__
    finally:
        conn.close()
    require(not fault and payload == DATA, 'fault let a complete GET through')
    return 'complete'


def emit(row, status, **fields):
    print(json.dumps(dict(law='N2', row=row, status=status, **fields)), flush=True)


def bucket_env(cert):
    env = dict(os.environ, HIRSEL_NET_REACTOR='epoll')
    if cert:
        env['SSL_CERT_FILE'] = str(cert)
        env.pop('SSL_CERT_DIR', None)
    return env


def run_bucket(fixture, mode, endpoint, region, bucket, prefix, access_id, cert, extra=(), timeout=30):
    cmd = [str(fixture), mode, endpoint, region, bucket, prefix, access_id, *extra]
    return subprocess.run(cmd, env=bucket_env(cert), capture_output=True, text=True, timeout=timeout)


def open_bucket_writer(fixture, endpoint, region, bucket, prefix, access_id, cert):
    cmd = [str(fixture), 'serve', endpoint, region, bucket, prefix, access_id]
    proc = subprocess.Popen(cmd, env=bucket_env(cert), stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    ready = proc.stdout.readline()
    require(ready == 'READY\n', f'bucket writer failed to open: {ready!r}')
    return proc


def send_op(proc, op, history=None):
    if history is not None and op != 0:
        history.invoke(op)
    try:
        proc.stdin.write(chr(op))
        proc.stdin.flush()
    except BrokenPipeError:
        line = ''
    else:
        line = proc.stdout.readline()
    if history is not None and op != 0:
        history.returned(op, line)
    return line


def close_proc(proc):
    if proc.poll() is None:
        try:
            proc.stdin.close()
        except OSError:
            pass
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def bucket_init(fixture, endpoint, region, bucket, prefix, access_id, cert, tmp, name):
    res = run_bucket(fixture, 'init', endpoint, region, bucket, prefix, access_id, cert)
    require(res.returncode == 0 and res.stdout.startswith('BASE '), f'bucket init failed: {res.stdout!r}')
    base = int(res.stdout.split()[1])
    return History(tmp / f'{name}.history', base)


def bucket_dump(fixture, endpoint, region, bucket, prefix, access_id, cert):
    res = run_bucket(fixture, 'dump', endpoint, region, bucket, prefix, access_id, cert)
    require(res.returncode == 0 and 'HEAD ' in res.stdout, f'bucket dump failed ({res.returncode}): {res.stdout!r}')
    return res.stdout


def bucket_minio(args, origin, bucket, prefix):
    fixture = Path(args.bucket_fixture).resolve()
    access_id = os.environ['AWS_ACCESS_KEY_ID']
    clean_ep = f'https://localhost:{args.port}'
    ca = args.ca or args.cert
    with tempfile.TemporaryDirectory(prefix='bucket-chaos-') as td:
        tmp = Path(td)

        # 1. Clean product Bucket Store round trip through HTTPS relay.
        sub = prefix + 'b-clean/'
        with running(args.port, args.cert, args.key, 'http', '', oneshot=False, ca=ca) as relay:
            ep = f'https://localhost:{relay.server_address[1]}'
            h = bucket_init(fixture, ep, 'us-east-1', bucket, sub, access_id, ca, tmp, 'clean')
            w = open_bucket_writer(fixture, ep, 'us-east-1', bucket, sub, access_id, ca)
            try:
                require('ACK ' in send_op(w, 1, h), 'bucket clean op 1')
                require('ACK ' in send_op(w, 2, h), 'bucket clean op 2')
                require('CUT' in send_op(w, 0), 'bucket clean cut')
                require('ACK ' in send_op(w, 3, h), 'bucket clean op 3')
            finally:
                close_proc(w)
            oracle = h.verify(bucket_dump(fixture, clean_ep, 'us-east-1', bucket, sub, access_id, ca))
            require(relay.observed.wait(2), 'zero native HTTP hits on bucket-clean')
            emit('bucket-clean', 'pass', scheduled='clean', native=relay.outcome, **oracle)

        # 2. Pre-forward 503 SlowDown (one-shot on PUT): Store retries and completes cleanly.
        sub = prefix + 'b-slowdown/'
        h = bucket_init(fixture, clean_ep, 'us-east-1', bucket, sub, access_id, ca, tmp, 'slowdown')
        with running(args.port, args.cert, args.key, 'http', '', oneshot=True, match='put', ca=ca) as relay:
            ep = f'https://localhost:{relay.server_address[1]}'
            w = open_bucket_writer(fixture, ep, 'us-east-1', bucket, sub, access_id, ca)
            try:
                relay.fault = 'slowdown'
                require('ACK ' in send_op(w, 1, h), 'bucket slowdown op 1')
                require('ACK ' in send_op(w, 2, h), 'bucket slowdown op 2')
            finally:
                close_proc(w)
            require(relay.observed.wait(2) and relay.outcome['status'] == 503, 'missing 503 hit')
            oracle = h.verify(bucket_dump(fixture, clean_ep, 'us-east-1', bucket, sub, access_id, ca))
            emit('bucket-slowdown', 'pass', scheduled='slowdown', native=relay.outcome, **oracle)

        # 3. Post-origin-CAS dropped reply (Q5): origin commits head CAS, relay drops 200 reply.
        sub = prefix + 'b-drop-cas/'
        h = bucket_init(fixture, clean_ep, 'us-east-1', bucket, sub, access_id, ca, tmp, 'drop-cas')
        with running(args.port, args.cert, args.key, 'http', '', oneshot=True, match='cas', ca=ca) as relay:
            ep = f'https://localhost:{relay.server_address[1]}'
            w = open_bucket_writer(fixture, ep, 'us-east-1', bucket, sub, access_id, ca)
            try:
                require('ACK ' in send_op(w, 1, h), 'bucket drop-cas baseline op 1')
                head_before = origin.head_object(Bucket=bucket, Key=sub + 'head')['ETag']
                relay.fault = 'drop-cas'
                send_op(w, 2, h)
            finally:
                close_proc(w)
            require(relay.observed.wait(2) and relay.outcome == dict(status=200, cas=True, dropped=True),
                    'missing Q5 drop-cas witness')
        head_after = origin.head_object(Bucket=bucket, Key=sub + 'head')['ETag']
        require(head_after != head_before, 'Q5 origin head did not advance')
        oracle = h.verify(bucket_dump(fixture, clean_ep, 'us-east-1', bucket, sub, access_id, ca))
        require(oracle['recovered'] == 2, f'Q5 committed op missing after recovery: {oracle}')
        emit('bucket-drop-cas', 'pass', scheduled='drop-cas', native=relay.outcome, q5_recovered=True, **oracle)

        # 4. Competing claim / real 412: Writer B adopts epoch, Writer A hits real 412 and is fenced.
        sub = prefix + 'b-claim-412/'
        h = bucket_init(fixture, clean_ep, 'us-east-1', bucket, sub, access_id, ca, tmp, 'claim')
        with running(args.port, args.cert, args.key, 'http', '', oneshot=True, match='cas', ca=ca) as relay:
            ep = f'https://localhost:{relay.server_address[1]}'
            wa = open_bucket_writer(fixture, ep, 'us-east-1', bucket, sub, access_id, ca)
            try:
                require('ACK ' in send_op(wa, 1, h), 'writer A op 1')
                wb = open_bucket_writer(fixture, clean_ep, 'us-east-1', bucket, sub, access_id, ca)
                try:
                    require('ACK ' in send_op(wb, 2, h), 'writer B op 2')
                finally:
                    close_proc(wb)
                relay.fault = 'claim'
                fenced_reply = send_op(wa, 3, h)
                require('ACK ' not in fenced_reply, f'fenced writer A unexpectedly acked: {fenced_reply!r}')
            finally:
                close_proc(wa)
            require(relay.observed.wait(2) and relay.outcome['status'] == 412 and relay.outcome['if_match'],
                    'missing real 412 witness')
        oracle = h.verify(bucket_dump(fixture, clean_ep, 'us-east-1', bucket, sub, access_id, ca))
        require(oracle['acked'] == 2 and oracle['recovered'] == 2, f'fenced op leaked: {oracle}')
        emit('bucket-claim-412', 'pass', scheduled='claim', native=relay.outcome, **oracle)

        # 5. Truncated & slowloris GET on Bucket Store reopen/dump (with checkpointed pack + entry).
        sub = prefix + 'b-read/'
        h = bucket_init(fixture, clean_ep, 'us-east-1', bucket, sub, access_id, ca, tmp, 'read')
        w = open_bucket_writer(fixture, clean_ep, 'us-east-1', bucket, sub, access_id, ca)
        try:
            require('ACK ' in send_op(w, 1, h), 'read seed op 1')
            require('ACK ' in send_op(w, 2, h), 'read seed op 2')
            require('CUT' in send_op(w, 0), 'read seed cut')
            require('ACK ' in send_op(w, 3, h), 'read seed op 3')
        finally:
            close_proc(w)
        for fault in ('truncate', 'slow-get'):
            with running(args.port, args.cert, args.key, 'http', fault, oneshot=True, match='get', ca=ca) as relay:
                ep = f'https://localhost:{relay.server_address[1]}'
                res = run_bucket(fixture, 'dump', ep, 'us-east-1', bucket, sub, access_id, ca)
                relay.stop.set()
                require(relay.observed.wait(3), f'zero native hits on bucket-{fault}')
                if res.returncode == 0:
                    oracle = h.verify(res.stdout)
                    recovered_on = 'retry'
                else:
                    require('HEAD ' not in res.stdout, f'corrupt dump succeeded on bucket-{fault}')
                    oracle = h.verify(bucket_dump(fixture, clean_ep, 'us-east-1', bucket, sub, access_id, ca))
                    recovered_on = 'reopen'
                emit('bucket-' + fault, 'pass', scheduled=fault, recovered_on=recovered_on,
                     native=relay.outcome, **oracle)

        # 6. Product std.object multipart completion faults (HTTP-200 <Error>, 409, 404 NoSuchUpload).
        for fault, want_status, want_code in (
            ('multipart-error', 200, 'InternalError'),
            ('conflict', 409, 'ConditionalRequestConflict'),
            ('no-upload', 404, 'NoSuchUpload'),
        ):
            with running(args.port, args.cert, args.key, 'http', fault, oneshot=False, match='complete', ca=ca) as relay:
                ep = f'https://localhost:{relay.server_address[1]}'
                res = run_bucket(fixture, 'multipart', ep, 'us-east-1', bucket, sub, access_id, ca,
                                 extra=('mp-' + fault,))
                require(res.returncode == 8 and 'MP_COMPLETE_ERR' in res.stdout,
                        f'std.object accepted faulted complete ({fault}): rc={res.returncode} out={res.stdout!r}')
                require(relay.observed.wait(2)
                        and relay.outcome == dict(status=want_status, code=want_code, forwarded=False),
                        f'missing native witness on bucket-{fault}: {relay.outcome}')
                emit('bucket-' + fault, 'pass', scheduled=fault, native=relay.outcome, client='MP_COMPLETE_ERR')

        # 7. Product Bucket Store over TCP-passthrough TLS relay (corrupt, handshake-stall, rst, fin, half-close).
        for fault in ('corrupt', 'handshake-stall', 'rst', 'fin', 'half-close'):
            with running(args.port, args.cert, args.key, 'tcp', fault, oneshot=True, ca=ca) as relay:
                ep = f'https://localhost:{relay.server_address[1]}'
                res = run_bucket(fixture, 'dump', ep, 'us-east-1', bucket, sub, access_id, ca)
                relay.stop.set()
                require(relay.observed.wait(3), f'zero native TCP hits on bucket-tcp-{fault}')
                if res.returncode == 0:
                    oracle = h.verify(res.stdout)
                    recovered_on = 'retry'
                else:
                    require('HEAD ' not in res.stdout, f'corrupt dump accepted on bucket-tcp-{fault}')
                    oracle = h.verify(bucket_dump(fixture, clean_ep, 'us-east-1', bucket, sub, access_id, ca))
                    recovered_on = 'reopen'
                emit('bucket-tcp-' + fault, 'pass', scheduled=fault, recovered_on=recovered_on,
                     native=relay.outcome, **oracle)


def bucket_tigris(args, bucket, prefix):
    fixture = Path(args.bucket_fixture).resolve()
    access_id = os.environ['AWS_ACCESS_KEY_ID']
    ep = 'https://fly.storage.tigris.dev'
    sub = prefix + 'b-tigris/'
    with tempfile.TemporaryDirectory(prefix='bucket-tigris-') as td:
        h = bucket_init(fixture, ep, 'auto', bucket, sub, access_id, None, Path(td), 'tigris')
        w = open_bucket_writer(fixture, ep, 'auto', bucket, sub, access_id, None)
        try:
            require('ACK ' in send_op(w, 1, h), 'tigris bucket op 1')
            require('ACK ' in send_op(w, 2, h), 'tigris bucket op 2')
            require('CUT' in send_op(w, 0), 'tigris bucket cut')
            require('ACK ' in send_op(w, 3, h), 'tigris bucket op 3')
        finally:
            close_proc(w)
        oracle = h.verify(bucket_dump(fixture, ep, 'auto', bucket, sub, access_id, None))
        emit('bucket-tigris-clean', 'pass', origin=bucket, prefix=sub, **oracle)


def minio(args, origin, obj, tag):
    ca = args.ca or args.cert
    for fault in ('clean', 'slowdown', 'drop-cas', 'claim', 'truncate', 'slow-get',
                  'multipart-error', 'conflict', 'no-upload'):
        with running(args.port, args.cert, args.key, 'http', fault, ca=ca) as relay:
            s3 = client(f'https://127.0.0.1:{relay.server_address[1]}', ca, 'us-east-1',
                        read_timeout=1 if fault == 'slow-get' else 12)
            if fault == 'clean':
                require(read(s3, obj) == DATA, 'signed relay control')
            elif fault == 'slowdown':
                require(expect_error(503, lambda: read(s3, obj)) == 'SlowDown', fault)
            elif fault == 'drop-cas':
                try:
                    put(s3, obj, b'claimed', 'If-Match', tag)
                except BotoCoreError:
                    pass
                else:
                    raise ValueError('CAS caller received a reply')
                require(read(origin, obj) == b'claimed', 'Q5 origin readback')
            elif fault == 'claim':
                expect_error(412, lambda: put(s3, obj, b'loser', 'If-Match', tag))
                require(read(origin, obj) == b'claimed', 'stale claim replaced winner')
                tag = origin.put_object(**obj, Body=DATA)['ETag']
            elif fault in ('truncate', 'slow-get'):
                stream = s3.get_object(**obj)['Body']
                with stream:
                    prefix = stream.read(len(DATA) // 2)
                    try:
                        stream.read()
                    except (BotoCoreError, OSError, http.client.HTTPException):
                        pass
                    else:
                        raise ValueError('short GET reported complete')
                with running(args.port, args.cert, args.key, 'http', 'resume', ca=ca) as resume:
                    retry = client(f'https://127.0.0.1:{resume.server_address[1]}', ca, 'us-east-1')
                    suffix = read(retry, obj, Range=f'bytes={len(prefix)}-', IfMatch=tag)
                    require(prefix + suffix == DATA and resume.observed.wait(2)
                            and resume.outcome == dict(status=206, range=f'bytes={len(prefix)}-', if_match=True),
                            'conditional Range resume')
            else:
                request = upload(origin, dict(obj, Key=obj['Key'] + '-fault'))
                try:
                    status, code = {'multipart-error': (200, 'InternalError'),
                                    'conflict': (409, 'ConditionalRequestConflict'),
                                    'no-upload': (404, 'NoSuchUpload')}[fault]
                    reported = expect_error(status, lambda: s3.complete_multipart_upload(**request))
                    if fault == 'multipart-error':
                        require(relay.observed.wait(2) and relay.outcome['code'] == code, fault)
                    else:
                        require(reported == code, fault)
                finally:
                    origin.abort_multipart_upload(**{k: v for k, v in request.items() if k != 'MultipartUpload'})
            relay.stop.set()
            require(relay.observed.wait(3), 'zero native HTTP hits')
            if fault == 'drop-cas':
                require(relay.outcome == dict(status=200, cas=True, dropped=True), 'missing CAS witness')
            emit(fault, 'pass', scheduled=fault, native=relay.outcome)
    signed = origin.generate_presigned_url('get_object', Params=obj)
    for fault in ('', 'corrupt', 'handshake-stall', 'rst', 'fin', 'half-close'):
        with running(args.port, args.cert, args.key, 'tcp', fault, ca=ca) as relay:
            outcome = transport(relay, ca, signed, fault)
            require(not fault or relay.observed.wait(2), 'zero native TCP hits')
            emit('tcp-' + (fault or 'clean'), 'pass', scheduled=fault, client=outcome, native=relay.outcome)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--tigris', action='store_true')
    p.add_argument('--port', type=int)
    p.add_argument('--ca')
    p.add_argument('--cert')
    p.add_argument('--key')
    p.add_argument('--bucket-fixture', type=Path)
    args = p.parse_args()
    if args.tigris:
        require(not any((args.port, args.ca, args.cert, args.key)), 'Tigris accepts clean controls only')
        bucket = 'divine-glitter-523'
        origin = client('https://fly.storage.tigris.dev', True, 'auto')
    else:
        bucket = 'store-chaos'
        origin = client(f'https://127.0.0.1:{args.port}', args.ca or args.cert, 'us-east-1')
        origin.create_bucket(Bucket=bucket)
    prefix = 'store-pitr-phase6/' + uuid.uuid4().hex + '/'
    obj = dict(Bucket=bucket, Key=prefix + 'head')
    try:
        tag = controls(origin, obj)
        emit('signed-control', 'pass', origin=bucket, prefix=prefix, http=[200, 412, 200, 412, 200, 204, 404])
        if args.tigris:
            if args.bucket_fixture:
                bucket_tigris(args, bucket, prefix)
        else:
            minio(args, origin, obj, tag)
            if args.bucket_fixture:
                bucket_minio(args, origin, bucket, prefix)
    finally:
        cleanup(origin, bucket, prefix)
        emit('cleanup', 'pass', origin=bucket, prefix=prefix, objects=0, uploads=0)


if __name__ == '__main__':
    try:
        main()
    except Exception as exc:
        import traceback
        print(' -> '.join(f.name for f in traceback.extract_tb(exc.__traceback__) if f.filename == __file__), file=sys.stderr)
        emit('qualification', 'fail', error=type(exc).__name__,
             reason=str(exc) if isinstance(exc, ValueError) else 'see failing operation')
        sys.exit(1)
