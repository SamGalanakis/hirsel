#!/usr/bin/env python3
"""SEM-10 check values for the std codec and Store declaration laws."""

import argparse
import hashlib
import struct


def u32(value):
    return struct.pack("<I", value)


def u64(value):
    return struct.pack("<Q", value)


def text(value):
    encoded = value.encode("utf-8")
    return u32(len(encoded)) + encoded


def digest(domain, data):
    encoded = domain.encode("utf-8")
    return hashlib.sha256(u64(len(encoded)) + encoded + u64(len(data)) + data).digest()


def node(name, data=b"", version=1):
    return text(name) + u32(version) + data


def builtin(name, data=b"", version=1, key=False):
    domain = "hirsel.key-storage.v1" if key else "hirsel.codec-storage.v1"
    return digest(domain, b"\x00" + node(name, data, version))


def group(*members):
    value = digest("hirsel.codec-group.v1", u32(len(members)) + b"".join(members))
    return [
        digest("hirsel.codec-storage.v1", b"\x01" + value + u32(i))
        for i in range(len(members))
    ]


def outside(fingerprint):
    return b"\x00" + fingerprint


def inside(member):
    return b"\x01" + u32(member)


def field(name, child, default=None, removed=False):
    fallback = b"\x00" if default is None else b"\x01" + u32(len(default)) + default
    return bytes([removed]) + text(name) + child + fallback


def variant(name, *payload, removed=False):
    return bytes([removed]) + text(name) + u32(len(payload)) + b"".join(payload)


def nominal(family, path, *entries):
    return node(family, text(path) + u32(len(entries)) + b"".join(entries))


U8 = outside(builtin("u8"))
U64 = outside(builtin("u64"))
I64 = outside(builtin("i64"))
STRING = outside(builtin("string"))
STD = "std@1/tests/codec/"
STORE = "store@1/store/"


def default_record(path, default):
    return group(nominal("record", path, field("v", U64), field("w", U64, u64(default))))[0]


def tree(path):
    # In both app and core/std tests, compare the full PROG-9 type texts.
    named = path + "DTree"
    wrapped = f"core@1/core/List[{named}]"
    order = sorted([named, wrapped])
    tree_id, list_id = order.index(named), order.index(wrapped)
    members = {
        named: nominal("record", named, field("label", U64), field("kids", inside(list_id))),
        wrapped: node("list", inside(tree_id)),
    }
    values = group(*(members[name] for name in order))
    return values[tree_id], values[list_id]


def expression(path):
    arg, expr = path + "StorageArg", path + "StorageExpr"
    args, optional = f"core@1/core/List[{arg}]", f"core@1/core/Option[{expr}]"
    order = sorted([arg, expr, args, optional])
    members = {
        arg: nominal(
            "sum", arg, variant("StoragePos", inside(order.index(expr))),
            variant("StorageNamed", STRING, inside(order.index(optional))),
        ),
        expr: nominal(
            "sum", expr, variant("StorageLit", I64),
            variant("StorageCall", STRING, inside(order.index(args))),
        ),
        args: node("list", inside(order.index(arg))),
        optional: node("option", inside(order.index(expr))),
    }
    values = group(*(members[name] for name in order))
    return values[order.index(arg)], values[order.index(expr)]


def vectors():
    row = nominal(
        "record", STD + "DTomb", field("id", U64),
        field("note", STRING, removed=True), field("name", STRING),
        field("score", I64, u64(7)),
    )
    shape = nominal(
        "sum", STD + "DCut", variant("Dot"), variant("Circle", outside(builtin("f64"))),
        variant("Rect", U64, U64, removed=True),
    )
    tree_value, list_value = tree(STD)
    arg, expr = expression(STD)
    wire = group(nominal("record", STD + "DRangeWire", field("lo", U64), field("hi", U64)))[0]
    return {
        "record": group(row)[0],
        "sum": group(shape)[0],
        "tree": tree_value,
        "tree_list": list_value,
        "arg": arg,
        "expr": expr,
        "bytes": builtin("list", U8),
        "key_string_1": builtin("key.string", key=True),
        "key_string_2": builtin("key.string", version=2, key=True),
        "key_desc": builtin("key.desc", outside(builtin("key.string", key=True)), key=True),
        "desc": builtin("desc", STRING),
        "default_7": default_record(STD + "StorageDefault", 7),
        "default_8": default_record(STD + "StorageDefault", 8),
        "store_default_7": default_record(STORE + "FingerprintDefault", 7),
        "store_default_8": default_record(STORE + "FingerprintDefault", 8),
        "via_prefix": u32(1) + node("via", text(STD + "DRange") + outside(wire)),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--via-revisions", nargs=2, metavar=("TO", "FROM"))
    args = parser.parse_args()
    values = vectors()
    if args.via_revisions:
        to, from_ = map(bytes.fromhex, args.via_revisions)
        if len(to) != 32 or len(from_) != 32:
            parser.error("each via revision must be exactly 32 bytes")
        raw = values["via_prefix"] + to + from_
        value = digest("hirsel.codec-group.v1", raw)
        values["via"] = digest("hirsel.codec-storage.v1", b"\x01" + value + u32(0))
    for name, value in values.items():
        print(name, value.hex())


if __name__ == "__main__":
    main()
