"""Prepare the Xiph RFC bitstreams and independent opus_demo expectations."""

from concurrent.futures import ThreadPoolExecutor
import filecmp
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile

ARCHIVES = (
    ("opus_testvectors.tar.gz", "opus_testvectors",
     "94ac78ca4f74c4e43bc9fe4ec1ad0aa36f38ab90f45b0727c40dd1e96096e767"),
    ("opus_testvectors-rfc8251.tar.gz", "opus_newvectors",
     "6b26a22f9ba87b2b836906a9bb7afec5f8e54d49553b1200382520ee6fedfa55"),
)


def fetch(work, filename, directory, expected):
    archive = work / filename
    if not archive.exists():
        partial = archive.with_suffix(".part")
        subprocess.run([
            "curl", "--fail", "--silent", "--show-error", "--location",
            f"https://opus-codec.org/static/testvectors/{filename}", "-o", partial,
        ], check=True)
        partial.rename(archive)
    with archive.open("rb") as source:
        if hashlib.file_digest(source, "sha256").hexdigest() != expected:
            raise SystemExit(f"{filename}: SHA-256 mismatch")
    with tarfile.open(archive) as source:
        source.extractall(work, filter="data")
    return work / directory


def run(*command):
    result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    if result.returncode:
        raise RuntimeError(f"{command}: {result.stdout}")
    return result.stdout


def prepare(reference, work, directory, suite, vector, channels):
    original = directory / f"testvector{vector:02}"
    stem = work / "fixtures" / f"{suite}-{vector}"
    source = stem.with_suffix(".bit")
    decoded = stem.with_suffix(".demo")
    audio = stem.with_suffix(".input")
    encoded = stem.with_suffix(".encoded")
    roundtrip = stem.with_suffix(".roundtrip")
    demo = reference / "opus_demo"
    compare = reference / "opus_compare"
    shutil.copyfile(original.with_suffix(".bit"), source)
    run(demo, "-d", "48000", str(channels), source, decoded)

    # Xiph accepts either bundled reference where the RFC ships two choices.
    alternatives = [original.with_suffix(".dec"), original.with_name(original.name + "m.dec")]
    flags = ["-s"] if channels == 2 else []
    comparisons = [
        subprocess.run([compare, *flags, path, decoded], text=True, capture_output=True)
        for path in alternatives if path.exists()
    ]
    scores = [
        float(re.search(r"Opus quality metric: ([0-9.]+)", result.stderr)[1])
        for result in comparisons if result.returncode == 0
    ]
    if not scores:
        raise RuntimeError(f"{source.name}: {''.join(result.stderr for result in comparisons)}")

    # Encode the first 400 ms of the reference-decoded audio with matching CTLs.
    width = 960 * channels * 2
    with decoded.open("rb") as pcm:
        frames = pcm.read(20 * width)
    audio.write_bytes(frames[:len(frames) // width * width])
    run(demo, "-e", "voip", "48000", str(channels), "48000", "-framesize", "20",
        "-complexity", "8", "-inbandfec", "-loss", "10", "-dtx", audio, encoded)
    run(demo, "-d", "48000", str(channels), encoded, roundtrip)
    return source.name, max(scores)


def stage(work):
    vectors = Path(__file__).resolve().parent
    codec = vectors.parent
    fixtures = work / "fixtures"
    fixtures.mkdir(parents=True, exist_ok=True)
    for filename in ("opus.hirsel", "native.hirsel", "libhirsel_opus.a"):
        shutil.copyfile(codec / filename, work / filename)
    for filename in ("tests.hirsel", "example.hirsel"):
        shutil.copyfile(vectors / filename, work / filename)

    # The test runs in its own empty working directory, so its grant is absolute.
    base = (codec / "hirsel.toml").read_text()
    tests = (vectors / "test.toml").read_text().replace(
        '"fixtures"', json.dumps(str(fixtures), ensure_ascii=False),
    )
    (work / "hirsel.toml").write_text(base + tests)


def main():
    work, reference = (Path(arg).resolve() for arg in sys.argv[1:])
    stage(work)
    original, updated = [fetch(work, *archive) for archive in ARCHIVES]
    # RFC 8251 §11 replaces the original PCM references but uses the same bitstreams.
    for stream in original.glob("*.bit"):
        if not filecmp.cmp(stream, updated / stream.name, shallow=False):
            raise RuntimeError(f"RFC bitstreams differ: {stream.name}")
    tasks = [(reference, work, updated, 2 - channels, n, channels)
             for channels in (2, 1) for n in range(1, 13)]
    with ThreadPoolExecutor(max_workers=4) as executor:
        for name, score in executor.map(lambda args: prepare(*args), tasks):
            print(f"{name}: opus_demo RFC quality {score:.1f}%", flush=True)


if __name__ == "__main__":
    main()
