"""Run the optional live S3 contract under one fresh prefix."""

import argparse
import datetime as dt
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from urllib.parse import urlsplit
from uuid import uuid4

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "core/tools"))
from omap_bench import inputs
from timing_shield import BOARD, REGIMES, hold


def command():
    args = argparse.ArgumentParser(description=__doc__)
    args.add_argument("--compiler", type=Path, required=True)
    args.add_argument("--runtime", type=Path, required=True)
    args.add_argument("--source", type=Path)
    args.add_argument("--scratch", type=Path, required=True)
    args.add_argument("--g7", action="store_true")
    return args.parse_args()


def cleanup(s3, name, prefix):
    for page in s3.get_paginator("list_multipart_uploads").paginate(Bucket=name, Prefix=prefix):
        for upload in page.get("Uploads", ()):
            key = upload["Key"]
            if not key.startswith(prefix):
                raise RuntimeError("S3 returned a multipart key outside the run prefix")
            s3.abort_multipart_upload(Bucket=name, Key=key, UploadId=upload["UploadId"])
    for page in s3.get_paginator("list_objects_v2").paginate(Bucket=name, Prefix=prefix):
        for entry in page.get("Contents", ()):
            key = entry["Key"]
            if not key.startswith(prefix):
                raise RuntimeError("S3 returned an object key outside the run prefix")
            s3.delete_object(Bucket=name, Key=key)
    if s3.list_objects_v2(Bucket=name, Prefix=prefix, MaxKeys=1).get("KeyCount"):
        raise RuntimeError("live smoke cleanup left objects")
    if s3.list_multipart_uploads(Bucket=name, Prefix=prefix, MaxUploads=1).get("Uploads"):
        raise RuntimeError("live smoke cleanup left multipart uploads")


def measure_knee(cmd, arguments, env, binary, results):
    subprocess.run([*cmd, "--backend", "llvm", "--release", "-o", binary], env=env, check=True)
    subprocess.run([binary, "seed", "8", *arguments], env=env, check=True)
    with hold(REGIMES, BOARD), results.open("x") as output:
        for lanes in (1, 2, 4, 8, 16, 32, 64, 128):
            now = dt.datetime.now().time()
            if dt.time(2, 45) <= now < dt.time(4):
                raise RuntimeError("local timing is closed from 02:45 to 04:00")
            elapsed = int(subprocess.check_output(
                ["taskset", "-c", "16", binary, "read", str(lanes), *arguments], env=env, text=True
            ))
            size = 64 * 8388608
            print(json.dumps({"lanes": lanes, "bytes": size, "ns": elapsed, "gb_per_s": size / elapsed}),
                  file=output, flush=True)
    return 0


def main():
    opts = command()
    file = Path.home() / ".config/hirsel/tigris-dev.env"
    if not file.is_file():
        print("Tigris smoke skipped because the credentials file is absent")
        return 0
    if os.environ.get("HIRSEL_TIGRIS_SMOKE") != "1":
        return subprocess.call([
            "bash", "-c", 'set -a; source "$1"; shift; export HIRSEL_TIGRIS_SMOKE=1; exec "$@"',
            "tigris-smoke", file, sys.executable, Path(__file__).resolve(), *sys.argv[1:],
        ])
    import boto3
    from botocore.config import Config

    endpoint = os.environ["AWS_ENDPOINT_URL_S3"]
    region = os.environ["AWS_REGION"]
    bucket = os.environ["BUCKET_NAME"]
    access = os.environ["AWS_ACCESS_KEY_ID"]
    if urlsplit(endpoint).hostname != "fly.storage.tigris.dev" or bucket != "divine-glitter-523":
        raise RuntimeError("the credential file must name the Tigris development fixture")
    prefix = f"std-object-1822/{uuid4().hex}/"
    s3 = boto3.client("s3", endpoint_url=endpoint, region_name=region,
                      config=Config(signature_version="s3v4", s3={"addressing_style": "path"}))
    opts.scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=opts.scratch) as temp:
        manifest = Path(temp) / "inputs.json"
        source = opts.source or HERE / ("object_g7.hirsel" if opts.g7 else "object_live.hirsel")
        document = inputs(source.resolve())
        document["packages"][-1]["package"]["name"] = "object_live"
        manifest.write_text(json.dumps(document))
        env = os.environ | {"HIRSEL_NET_REACTOR": "epoll"}
        cmd = [
            opts.compiler, "build" if opts.g7 else "run", "--source-core", "--source-std",
            "--inputs", manifest, "--entry", "object_live@1/main/main",
            "--runtime", opts.runtime, "--limit", "1073741824" if opts.g7 else "134217728",
            "--store", opts.scratch.resolve() / "live-store",
            "--grant", "Clock", "--grant", "Random", "--grant", "Stdout", "--grant", "Tasks",
            "--grant", "Secret:root=env:AWS_SECRET_ACCESS_KEY kdf",
            "--grant", "Net=Resolve fly.storage.tigris.dev;Connect 0.0.0.0/0 443-443;Connect ::/0 443-443;Tls fly.storage.tigris.dev 443-443 System",
        ]
        args = [endpoint, region, bucket, prefix, access]
        ok = None
        try:
            if opts.g7:
                ok = measure_knee(cmd, args, env, Path(temp) / "g7", opts.scratch / "g7.jsonl")
            else:
                ok = subprocess.run([*cmd, "--", *args], env=env).returncode
        finally:
            cleanup(s3, bucket, prefix)
        return ok


if __name__ == "__main__":
    sys.exit(main())
