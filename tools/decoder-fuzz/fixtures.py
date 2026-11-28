#!/usr/bin/env python3
"""Project reviewed v1 inputs for an ordinary decoder law."""
import argparse
import hashlib
import json
from pathlib import Path


def unique(pairs):
    values = dict(pairs)
    if len(values) != len(pairs):
        raise ValueError("duplicate JSON key")
    return values


def keys(value, *names):
    if not isinstance(value, dict) or set(value) != set(names):
        raise ValueError(f"expected fields {', '.join(names)}")


def number(value, low, high):
    if type(value) is not int or not low <= value <= high:
        raise ValueError(f"expected integer in {low}..{high}, got {value!r}")
    return value


def select(path, target, law):
    bundle = json.loads(path.read_bytes(), object_pairs_hook=unique)
    version = bundle.get("schema_version")
    if type(version) is not int or version != 1:
        raise ValueError(f"unsupported reviewed schema version {version!r}")
    keys(bundle, "schema_version", "objects", "cases")
    objects = bundle["objects"]
    for digest, encoded in objects.items():
        raw = bytes.fromhex(encoded)
        if raw.hex() != encoded or hashlib.sha256(raw).hexdigest() != digest:
            raise ValueError(f"invalid reviewed object {digest}")
    selected = []
    previous = ""
    used = set()
    for case in bundle["cases"]:
        keys(case, "id", "target", "law", "input_kind", "generator_version", "digest",
             "limits", "setup", "oracle", "expected", "schedules")
        case_id = case["id"]
        if not isinstance(case_id, str) or case_id <= previous:
            raise ValueError(f"duplicate, empty or unsorted case ID {case_id!r}")
        previous = case_id
        version = case["generator_version"]
        if type(version) is not int or version != 1:
            raise ValueError(f"{case_id}: stale generator version {version!r}")
        if case["digest"] not in objects:
            raise ValueError(f"{case_id}: missing reviewed object")
        used.add(case["digest"])
        keys(case["oracle"], "name", "version")
        if (not all(isinstance(case[k], str) and case[k] for k in ("target", "law", "input_kind"))
                or not all(isinstance(case[k], dict) for k in ("limits", "setup", "expected"))
                or not isinstance(case["schedules"], list) or not case["schedules"]
                or not isinstance(case["oracle"]["name"], str) or not case["oracle"]["name"]):
            raise ValueError(f"{case_id}: invalid reviewed case")
        number(case["oracle"]["version"], 1, 2**32 - 1)
        if (case["target"], case["law"]) == (target, law):
            selected.append(case)
    if used != objects.keys():
        raise ValueError("unreferenced reviewed object")
    if not selected:
        raise ValueError(f"no reviewed cases for {target}/{law}")
    return dict(schema_version=1, cases=selected,
                objects={key: objects[key] for key in sorted({c["digest"] for c in selected})})


def partition(schedule, size):
    if set(schedule) == {"ends"}:
        ends = schedule["ends"]
        if not isinstance(ends, list) or not ends or ends[-1] != size:
            raise ValueError("partition must cover input")
        for end in ends:
            number(end, 0, size)
        if ends != sorted(ends):
            raise ValueError("partition ends are out of order")
        return ends
    keys(schedule, "seed", "max_chunk")
    state = number(schedule["seed"], 0, 2**32 - 1)
    width = number(schedule["max_chunk"], 1, max(size, 1))
    ends = [0]
    while ends[-1] < size:
        state = (1664525 * state + 1013904223) & 0xffffffff
        ends.append(min(size, ends[-1] + 1 + state % width))
    return ends


def literal(raw):
    return 'b"' + ''.join(f'\\x{byte:02x}' for byte in raw) + '"'


def http_body(case, raw):
    keys(case["limits"], "head_bytes")
    number(case["limits"]["head_bytes"], 32768, 32768)
    if (case["input_kind"] != "request-head" or case["setup"] != {"parser": "request"}
            or case["oracle"] != {"name": "http1-request-head", "version": 1}):
        raise ValueError("wrong HTTP input kind, setup or oracle")
    expected = case["expected"]
    keys(expected, "status", "consumed", "end", "method", "target", "version", "fields")
    status = expected["status"]
    if status not in ("Complete", "Partial", "Bad", "TooLarge"):
        raise ValueError("unknown HTTP progress")
    end = number(expected["end"], 0, len(raw))
    consumed = number(expected["consumed"], 0, len(raw))
    fields = expected["fields"]
    if not isinstance(fields, list) or any(not isinstance(f, list) or len(f) != 2 for f in fields):
        raise ValueError("expected HTTP field pairs")
    lines = ['fn reviewed_input() -> bool {', f'    let raw = {literal(raw)}',
             '    let mut names: List[Bytes] = list.new()', '    let mut values: List[Bytes] = list.new()']
    for name, value in fields:
        lines.append(f'    list.push(mut names, {literal(name.encode())})')
        lines.append(f'    list.push(mut values, {literal(value.encode())})')
    def option(key):
        value = expected[key]
        return f'Some({literal(value.encode())})' if value is not None else 'None'
    version = expected["version"]
    version = 'None' if version is None else f'Some({number(version, 0, 1)}u8)'
    lines += [f'    if !request_case(raw, {status}, {end}, {option("method")}, {option("target")}, {version}, names, values) {{',
              '        return false', '    }']
    for index, schedule in enumerate(case["schedules"]):
        ends = partition(schedule, len(raw))
        lines.append(f'    let mut ends_{index}: List[u64] = list.new()')
        lines.extend(f'    list.push(mut ends_{index}, {n})' for n in ends)
        lines += [f'    if !reviewed_fragmentation(raw, ends_{index}, {status}, {end}, {consumed}) {{',
                  '        return false', '    }']
    return '\n'.join(lines + ['    true', '}', ''])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    project = commands.add_parser("http1")
    project.add_argument("bundle", type=Path)
    args = parser.parse_args()
    if args.command == "http1":
        bundle = select(args.bundle, "http1", "reviewed_fragmentation")
        rows = []
        for case in bundle["cases"]:
            try:
                body = http_body(case, bytes.fromhex(bundle["objects"][case["digest"]]))
            except (ValueError, TypeError, AttributeError) as error:
                raise ValueError(f"{case['id']}: {error}") from error
            rows.append(dict(id=case["id"], source=body))
        print(json.dumps(rows, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError) as error:
        raise SystemExit(str(error))
