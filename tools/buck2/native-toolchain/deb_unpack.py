"""Extract a Debian package's data archive on Linux or macOS."""

from pathlib import Path, PurePosixPath
import ctypes
import os
import subprocess
import sys
import tarfile
import tempfile


TAR_MODES = {
    b"data.tar": "r:",
    b"data.tar.gz": "r:gz",
    b"data.tar.bz2": "r:bz2",
    b"data.tar.xz": "r:xz",
    b"data.tar.zst": "r:zst",
}


def member_size(field):
    value = field.strip()
    if not value.isdigit():
        raise ValueError("invalid ar member size")
    return int(value)


def data_member(archive):
    end = archive.seek(0, os.SEEK_END)
    archive.seek(0)
    if archive.read(8) != b"!<arch>\n":
        raise ValueError("invalid ar signature")
    names = b""
    payload = None
    while archive.tell() < end:
        header = archive.read(60)
        if len(header) != 60 or header[58:] != b"`\n":
            raise ValueError("invalid ar member header")
        size = member_size(header[48:58])
        offset = archive.tell()
        next_header = offset + size + size % 2
        if next_header > end:
            raise ValueError("truncated ar member")
        name = header[:16].rstrip(b" ")
        if name == b"//":
            names = archive.read(size)
        elif name.startswith(b"#1/"):
            length = member_size(name[3:])
            if length > size:
                raise ValueError("truncated BSD ar name")
            name = archive.read(length).rstrip(b"\0")
        elif name[1:].isdigit() and name.startswith(b"/"):
            index = int(name[1:])
            if index >= len(names) or (index and names[index - 1:index] != b"\n"):
                raise ValueError("invalid GNU ar name offset")
            name = names[index:].split(b"\n", 1)[0].removesuffix(b"/")
        else:
            name = name.removesuffix(b"/")
        if name in TAR_MODES:
            if payload is not None:
                raise ValueError("multiple data archives in deb")
            payload = (TAR_MODES[name], archive.tell(), offset + size - archive.tell())
        if size % 2:
            archive.seek(next_header - 1)
            if archive.read(1) != b"\n":
                raise ValueError("invalid ar member padding")
        archive.seek(next_header)
    if payload is None:
        raise ValueError("no supported data archive in deb")
    return payload


def filter_member(member, destination, rooted_links):
    name = PurePosixPath(member.name)
    if name.is_absolute() or ".." in name.parts or (name == PurePosixPath(".") and not member.isdir()):
        raise ValueError(f"unsafe deb member path: {member.name!r}")
    if member.issym() or member.islnk():
        link = PurePosixPath(member.linkname)
        if member.issym() and link.is_absolute() and rooted_links:
            # Some pinned usr-merged Debian packages name /lib in the package root.
            relative = os.path.relpath(member.linkname.lstrip("/") or ".", name.parent)
            member = member.replace(linkname=relative)
        if member.islnk():
            source = Path(destination) / link
            if link.is_absolute() or ".." in link.parts or source.is_symlink() or not source.is_file():
                raise ValueError(f"unsafe or missing deb hardlink target: {member.linkname!r}")

    filtered = tarfile.data_filter(member, destination)
    # A hardlink shares the original file's mode and timestamps.
    return filtered.replace(mode=None, mtime=None) if member.islnk() else filtered


def extract_tar(data, mode, destination, rooted_links):
    with tarfile.open(fileobj=data, mode=mode) as archive:
        archive.extractall(destination, filter=lambda member, path: filter_member(member, path, rooted_links))


def decode_zstd(data, decoded):
    # The Linux worker has libzstd but no zstd command. The CLI also works
    # on hosts that do not expose the shared library to Python's loader.
    library = "libzstd.so.1" if sys.platform.startswith("linux") else "libzstd.1.dylib"
    try:
        zstd = ctypes.CDLL(library)
    except OSError:
        subprocess.run(["zstd", "-dq"], stdin=data, stdout=decoded, check=True)
        return

    class Buffer(ctypes.Structure):
        _fields_ = [("data", ctypes.c_void_p), ("size", ctypes.c_size_t), ("pos", ctypes.c_size_t)]

    zstd.ZSTD_createDStream.restype = ctypes.c_void_p
    zstd.ZSTD_initDStream.argtypes = [ctypes.c_void_p]
    zstd.ZSTD_initDStream.restype = ctypes.c_size_t
    zstd.ZSTD_freeDStream.argtypes = [ctypes.c_void_p]
    zstd.ZSTD_freeDStream.restype = ctypes.c_size_t
    zstd.ZSTD_decompressStream.argtypes = [ctypes.c_void_p, ctypes.POINTER(Buffer), ctypes.POINTER(Buffer)]
    zstd.ZSTD_decompressStream.restype = ctypes.c_size_t
    zstd.ZSTD_isError.argtypes = [ctypes.c_size_t]
    zstd.ZSTD_isError.restype = ctypes.c_uint
    zstd.ZSTD_getErrorName.argtypes = [ctypes.c_size_t]
    zstd.ZSTD_getErrorName.restype = ctypes.c_char_p

    def checked(result):
        if zstd.ZSTD_isError(result):
            raise ValueError(f"invalid zstd archive: {zstd.ZSTD_getErrorName(result).decode()}")
        return result

    stream = zstd.ZSTD_createDStream()
    if not stream:
        raise MemoryError("could not allocate a zstd decoder")
    try:
        remaining = checked(zstd.ZSTD_initDStream(stream))
        incoming = ctypes.create_string_buffer(128 * 1024)
        outgoing = ctypes.create_string_buffer(128 * 1024)
        output = Buffer(ctypes.addressof(outgoing), len(outgoing), 0)
        while size := data.readinto(incoming):
            source = Buffer(ctypes.addressof(incoming), size, 0)
            while source.pos < source.size or output.pos == output.size:
                output.pos = 0
                remaining = checked(zstd.ZSTD_decompressStream(stream, ctypes.byref(output), ctypes.byref(source)))
                decoded.write(memoryview(outgoing).cast("B")[:output.pos])
                if remaining == 0 and source.pos == source.size:
                    break
        if remaining:
            raise ValueError("truncated zstd archive")
    finally:
        zstd.ZSTD_freeDStream(stream)


def unpack_deb(path, destination, *, rooted_links=False):
    """Extract into a private staging directory, optionally rebasing absolute symlinks."""
    with open(path, "rb") as archive, tempfile.TemporaryFile() as data:
        mode, offset, size = data_member(archive)
        archive.seek(offset)
        while size:
            chunk = archive.read(min(size, 1 << 20))
            if not chunk:
                raise ValueError("truncated deb data archive")
            data.write(chunk)
            size -= len(chunk)
        data.seek(0)
        if mode == "r:zst" and sys.version_info < (3, 14):
            with tempfile.TemporaryFile() as decoded:
                decode_zstd(data, decoded)
                decoded.seek(0)
                extract_tar(decoded, "r:", destination, rooted_links)
        else:
            extract_tar(data, mode, destination, rooted_links)
    root = Path(destination).resolve()
    for path in root.rglob("*"):
        if path.is_symlink() and not path.resolve().is_relative_to(root):
            raise ValueError(f"deb link escapes its root: {path}")
