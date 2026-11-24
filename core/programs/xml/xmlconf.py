#!/usr/bin/env python3
"""Run the XML package's law against the extracted W3C xmlconf 20130923 suite."""
import argparse
from collections import Counter
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET


PACKAGE = Path(__file__).resolve().parent
LIBRARIES = PACKAGE.parents[1]
BASE = "{http://www.w3.org/XML/1998/namespace}base"
DECL = re.compile(rb"<\?xml\s+.*?\?>", re.DOTALL)
ENTITY = re.compile(rb'<!ENTITY\s+([\w.-]+)\s+SYSTEM\s+"([^"]+)">')
SUBSET = re.compile(rb"<!DOCTYPE TESTSUITE.*?\]\s*>", re.DOTALL)
DTD = re.compile(rb"(<!--.*?-->|<\?.*?\?>)|<!DOCTYPE", re.DOTALL)
INTERNAL = re.compile(rb"<!DOCTYPE[ \t\r\n]+[^\s>\[]+[ \t\r\n]*\[(.*?)\][ \t\r\n]*>", re.S)
ELEMENT = re.compile(rb"<!ELEMENT[ \t\r\n]+[^%<>]+>")
ATTLIST = re.compile(
    rb"<!ATTLIST[ \t\r\n]+[^\s%<>]+(?:[ \t\r\n]+[^\s%<>]+"
    rb"[ \t\r\n]+CDATA[ \t\r\n]+#(?:REQUIRED|IMPLIED))*[ \t\r\n]*>"
)
COMMENT = re.compile(rb"<!--.*?-->", re.S)

LAW = """
fn record_failure(mut failures: String, id: String, complaint: String) {
    string.push(mut failures, id)
    string.push(mut failures, " ")
    string.push(mut failures, complaint)
    string.push(mut failures, "\\n")
}

fn reject(mut failures: String, id: String, take input: Bytes) {
    match xml.parse(take input, xml.defaults()) {
        Error(Syntax) => {},
        Error(BadNamespace) => {},
        Error(BadCharacter) => {},
        Error(BadEncoding) => {},
        Error(UnsupportedEncoding) => {},
        Error(UnknownEntity) => {},
        Error(DtdForbidden) => {},
        Error(_) => record_failure(mut failures, id, "resource limit"),
        Ok(_) => record_failure(mut failures, id, "accepted malformed document"),
    }
}

fn accept(mut failures: String, id: String, take input: Bytes, expected: Option[Bytes]) {
    match xml.parse(take input, xml.defaults()) {
        Ok(take doc) => {
            if let Some(want) = expected {
                match xml.canonical(doc, xml.defaults()) {
                    Ok(actual) => {
                        if actual != want {
                            record_failure(mut failures, id, "canonical output mismatch")
                        }
                    },
                    Error(_) => record_failure(mut failures, id, "writer error"),
                }
            }
        },
        Error(BadEncoding) => record_failure(mut failures, id, "bad encoding"),
        Error(UnsupportedEncoding) => record_failure(mut failures, id, "unsupported encoding"),
        Error(BadNamespace) => record_failure(mut failures, id, "bad namespace"),
        Error(DtdForbidden) => record_failure(mut failures, id, "DTD refused"),
        Error(_) => record_failure(mut failures, id, "valid input refused"),
    }
}
"""


def cases(suite):
    catalogue = (suite / "xmlconf.xml").read_bytes()
    subset = SUBSET.search(catalogue)
    if subset is None:
        raise ValueError("xmlconf.xml has no catalogue entity map")
    parts = {
        name: DECL.sub(b"", (suite / path.decode("ascii")).read_bytes(), count=1)
        for name, path in ENTITY.findall(subset[0])
    }
    # Inline only the known catalogue fragments. ElementTree never reads test DTDs.
    merged = re.sub(rb"&([\w.-]+);", lambda match: parts.get(match[1], match[0]),
                    SUBSET.sub(b"", catalogue, count=1))

    # The 2013 catalogue gives the miscellaneous fragment an obsolete xml:base.
    merged = merged.replace(b'xml:base="eduni/namespaces/misc/"', b'xml:base="eduni/misc/"')

    def walk(node, base):
        if value := node.get(BASE):
            base = base / value
        if node.tag == "TEST":
            yield node.attrib, base
        for child in node:
            yield from walk(child, base)

    yield from walk(ET.fromstring(merged), suite)


def markup(data):
    """Project ASCII-compatible markup to bytes with a reversible offset map."""
    if data.startswith(b"\xff\xfe") or data.startswith(b"<\x00?\x00"):
        return data[::2], 2, 0
    if data.startswith(b"\xfe\xff") or data.startswith(b"\x00<\x00?"):
        return data[1::2], 2, 1
    return data, 1, 0


def doctype(data):
    return next((m.start() for m in DTD.finditer(data) if m[1] is None), None)


def trim_dtd(data):
    probe, width, offset = markup(data)
    at = doctype(probe)
    if at is None:
        return data
    match = INTERNAL.match(probe, at)
    if match is None:
        return None
    # Element declarations and CDATA attributes without defaults do not
    # change Clark canonical output. Preserve all other bytes verbatim.
    subset = COMMENT.sub(b"", match[1]).strip()
    while subset:
        declaration = ELEMENT.match(subset) or ATTLIST.match(subset)
        if declaration is None:
            return None
        subset = subset[declaration.end():].strip()
    start, end = match.start() * width, match.end() * width
    if width == 2:
        other = data[1 - offset::2]
        # Verify the removed markup uses ASCII code units.
        if any(other[match.start():match.end()]):
            return None
    return data[:start] + data[end:]


def select(attributes):
    kind = attributes["TYPE"]
    recommendation = attributes.get("RECOMMENDATION", "XML1.0")
    if recommendation in {"XML1.1", "NS1.1"} or "1.0" not in attributes.get("VERSION", "1.0").split():
        return "XML or Namespaces 1.1 only"
    if "5" not in attributes.get("EDITION", "5").split():
        return "earlier XML edition only"
    if kind not in {"valid", "not-wf"}:
        return "validation-only or discretionary diagnostic"
    if kind == "valid" and attributes.get("NAMESPACE", "yes") == "no":
        return "namespace processing disabled"
    return None


def literal(data):
    escapes = {34: r'\"', 92: r'\\', 9: r'\t', 10: r'\n', 13: r'\r'}
    encoded = "".join(
        escapes[byte] if byte in escapes else chr(byte) if 32 <= byte < 127 else f"\\x{byte:02x}"
        for byte in data
    )
    return f'b"{encoded}"'


def prepare(suite, destination, inventory):
    assertions = []
    skips = []
    counts = Counter()
    for attrs, base in cases(suite):
        id_ = attrs["ID"]
        kind = attrs["TYPE"]
        data = (base / attrs["URI"]).read_bytes()
        if reason := select(attrs):
            skips.append((reason, id_, attrs["URI"]))
            continue
        if kind == "not-wf":
            assertions.append(f'    reject(mut failures, "{id_}", {literal(data)})')
            counts["not-wf"] += 1
            if doctype(markup(data)[0]) is not None:
                counts["not-wf with DTD"] += 1
        else:
            trimmed = trim_dtd(data) if attrs.get("ENTITIES", "none") == "none" else None
            if trimmed is None:
                skips.append(("DTD or external entity required", id_, attrs["URI"]))
                continue
            if len(trimmed) != len(data):
                counts["valid with declaration-only DTD removed"] += 1
            expected = "None"
            if "OUTPUT" in attrs:
                expected = f'Some({literal((base / attrs["OUTPUT"]).read_bytes())})'
                counts["canonical comparisons"] += 1
            assertions.append(f'    accept(mut failures, "{id_}", {literal(trimmed)}, {expected})')
            counts["valid"] += 1

    if not counts["not-wf"] or not counts["valid"] or not counts["canonical comparisons"]:
        raise ValueError("catalogue supplied no negative, valid or canonical evidence")
    inventory.write_text("reason\tid\turi\n" + "".join(
        "\t".join(row) + "\n" for row in sorted(skips)
    ))
    for source in PACKAGE.iterdir():
        if source.suffix == ".hirsel" or source.name == "hirsel.toml":
            shutil.copyfile(source, destination / source.name)

    with (destination / "laws.hirsel").open("a") as output:
        output.write(LAW)
        batches = range(0, len(assertions), 24)
        for index in batches:
            output.write(f"\nfn batch_{index}(mut failures: String) {{\n")
            output.write("\n".join(assertions[index:index + 24]))
            output.write("\n}\n")
        output.write("\n@test\nfn xmlconf() -> Result[(), ()] {\n")
        output.write('    let mut failures = string.new()\n')
        for index in batches:
            output.write(f"    batch_{index}(mut failures)\n")
        output.write('    if string.byte_len(failures) != 0 {\n        fail(take failures)\n    }\n')
        output.write("    xml_contract()\n}\n")

    print(f"Selected {len(assertions)}/{len(assertions) + len(skips)} cases", flush=True)
    for name, count in sorted(counts.items()):
        print(f"  {name}: {count}", flush=True)
    for reason, count in sorted(Counter(row[0] for row in skips).items()):
        print(f"  skipped {reason}: {count}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("suite", type=Path, help="directory containing xmlconf.xml")
    parser.add_argument("--hirsel", default="hirsel")
    parser.add_argument("--inventory", required=True, type=Path, help="write skipped IDs and reasons here")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="xmlconf-") as directory:
        destination = Path(directory)
        prepare(args.suite, destination, args.inventory)
        return subprocess.call([args.hirsel, "test", directory, "--libraries", str(LIBRARIES),
                                "--exact", "xml@1/tests/xmlconf", "--test-timeout", "300", "--replay"])


if __name__ == "__main__":
    raise SystemExit(main())
