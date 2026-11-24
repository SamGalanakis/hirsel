# Archive

`tar.read` requests contiguous ranges and emits a member followed by chunks of
its contents. `tar.write` sorts names by unsigned byte order and requests each
member's data by its index in the original input list. `tar.file` sets mode 0644,
zero ownership and empty owner names. Supply the mtime explicitly. The reader
and writer accept USTAR and PAX path, linkpath, size and nanosecond mtime.

`zip.read` uses a seekable source and delivers members in physical order. It
checks the directory against the local headers and rejects intersecting
records before decoding data. `zip.read_local` accepts a forward-only source
ending after the last local record. It can find Deflate and Zstd descriptor
boundaries and buffers those entries within the configured bounds. A stored
entry with a descriptor needs its size in a local header or central directory.
Unix mode information is available only through the central directory.
`zip.write` emits Store, Deflate or Zstd method 93 in input order, and
automatically uses ZIP64 at the format's size, offset or count limits.

Both readers use `limits.defaults()` or caller-supplied limits. Pass depth
zero for the outer archive and increment it when opening an embedded archive.
Nested files are emitted as data. Source callbacks must supply exact ranges
from one immutable archive. A failed call can leave a prefix in receiver or
writer callbacks. Commit extracted files only after a successful return.
Resolve filesystem paths beneath the extraction root without following
existing symlinks. The readers reject absolute and parent-relative symlink
targets. TAR hard links and special file types, encrypted ZIP entries and
multi-disk ZIP files are unsupported.

## Fixture sources

The literals in `fixtures.hirsel` are generated archives containing data
created for this test in this repository. They use the repository's license.
GNU tar 1.35 is GPL-3.0-or-later, bsdtar 3.7.2 is BSD-licensed, Info-ZIP
Zip 3.0 has the Info-ZIP license, and CPython 3.14 is PSF-licensed. The
fixtures contain archive bytes produced by those programs.

In an empty input directory, create the fixture files, then run these
commands. The TAR blocking factor removes extra terminal padding. bsdtar
can record the input filesystem's atime and ctime. Those values do not
affect the conformance check.

```sh
python3.14 - <<'PY'
from pathlib import Path
import os

entries = {
    "a.txt": b"archive interop\n",
    "p" * 110 + ".txt": b"long name\n",
    "compressed.bin": bytes(range(256)) * 150,
}
for name, data in entries.items():
    Path(name).write_bytes(data)
    os.utime(name, ns=(1700000000123456789, 1700000000123456789))
PY

tar -b 1 --format=ustar --owner=0 --group=0 --mtime=@0 -cf ../gnu-ustar.tar a.txt
tar -b 1 --format=posix --pax-option=delete=atime,delete=ctime \
    --owner=0 --group=0 --mtime=@-0.25 -cf ../gnu-pax.tar p*.txt
bsdtar -b 1 --format=ustar --uid=0 --gid=0 -cf ../bsd-ustar.tar a.txt
bsdtar -b 1 --format=pax --uid=0 --gid=0 -cf ../bsd-pax.tar p*.txt
zip -Xq -Z deflate ../info-zip.zip compressed.bin
```

Create the Python ZIP fixtures with CPython 3.14. The short forward-only
fixtures end just before the central directory.

```python
from io import BytesIO
from pathlib import Path
from zipfile import ZipFile, ZipInfo, ZIP_DEFLATED, ZIP_STORED, ZIP_ZSTANDARD

text = Path("a.txt").read_bytes()
plain = Path("compressed.bin").read_bytes()

class ForwardOnly(BytesIO):
    def seekable(self):
        return False

    def seek(self, *args):
        raise OSError("forward-only sink")

def descriptor_zip(include_store):
    wire = ForwardOnly()
    with ZipFile(wire, "w") as archive:
        if include_store:
            archive.writestr("a.txt", text, compress_type=ZIP_STORED)
        member = ZipInfo("compressed.bin")
        member.compress_type = ZIP_DEFLATED
        with archive.open(member, "w", force_zip64=True) as entry:
            entry.write(plain)
        archive.writestr("z.txt", text, compress_type=ZIP_ZSTANDARD)
    return wire.getvalue()

def local_records(wire):
    return wire[:wire.index(b"PK\x01\x02")]

Path("../python.zip").write_bytes(descriptor_zip(True))
Path("../local-descriptors.zip").write_bytes(
    local_records(descriptor_zip(False)))

wire = BytesIO()
with ZipFile(wire, "w") as archive:
    archive.writestr("a.txt", text)
inner = wire.getvalue()
with ZipFile("../nested.zip", "w") as archive:
    archive.writestr("inner.zip", inner)
    archive.writestr("b.txt", text)
Path("../local.zip").write_bytes(local_records(inner))
```

The `python_checked_*` fixtures are outputs of `written_tar(false)`,
`written_zip(false)` and `written_zip(true)` from the law. CPython 3.14
`tarfile.extractfile` verified the TAR names, bytes, mtimes and zero
ownership. `zipfile.read` and `testzip` verified both ZIP layouts and
methods 0, 8 and 93. The law also writes the TAR in reversed input order and
checks that the bytes match.
