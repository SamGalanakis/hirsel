"""Bucket sampled B1 user instructions by the innermost attributable phase."""

from collections import Counter
from dataclasses import dataclass
import json
from pathlib import Path
import re
import subprocess

PHASES = ("Parse", "Route", "Handler", "Response write", "Scheduler and reactor",
          "Accounting and charges", "Copies", "Unattributed")

PERF_FRAME = re.compile(r"^\s*([0-9a-f]+) (.*?) \((.*)\+0x([0-9a-f]+)\)$")
FUNCTION = re.compile(r"\bfn ([a-zA-Z0-9_]+)\s*(?:\[|\()")
CRATE_HASH = re.compile(r"\[[0-9a-f]{16}\]")


@dataclass(frozen=True)
class Frame:
    pc: int
    symbol: str
    binary: str
    offset: int


def read_profile(path):
    samples = Counter()
    command = ["perf", "script", "--no-inline", "-i", str(path),
               "-F", "period,event,ip,sym,dso,dsoff"]
    with subprocess.Popen(command, text=True, stdout=subprocess.PIPE) as perf:
        frames = []
        period = 0
        for line in perf.stdout:
            if " instructions:u:" in line:
                if frames:
                    samples[tuple(frames)] += period
                frames = []
                period = int(line.split()[0])
            elif match := PERF_FRAME.match(line):
                pc, symbol, binary, offset = match.groups()
                frames.append(Frame(int(pc, 16), symbol, binary, int(offset, 16)))
            elif line.strip().endswith("[unknown] ([unknown])"):
                pc = int(line.split()[0], 16)
                frames.append(Frame(pc, "[unknown]", "", 0))
        if frames:
            samples[tuple(frames)] += period
        if perf.wait() or not samples:
            raise RuntimeError(f"no usable instruction call chains in {path}")
    return samples


def resolve_functions(samples, binary):
    binary = str(binary.resolve())
    segments = []
    for line in subprocess.check_output(["readelf", "-Wl", binary], text=True).splitlines():
        fields = line.split()
        if fields and fields[0] == "LOAD":
            offset, address = (int(s, 16) for s in fields[1:3])
            segments.append((offset, address, int(fields[4], 16)))
    offsets = {frame.offset for stack in samples for frame in stack if frame.binary == binary}

    def address_for_file_offset(offset):
        for start, address, size in segments:
            if start <= offset < start + size:
                return address + offset - start
        raise ValueError(f"unmapped executable offset {offset:x} in {binary}")

    offsets = sorted(offsets)
    addresses = [address_for_file_offset(offset) for offset in offsets]
    results = subprocess.run(["llvm-symbolizer", f"--obj={binary}", "--inlines",
                              "--output-style=JSON"],
                             input="".join(f"{address:#x}\n" for address in addresses),
                             capture_output=True, text=True, check=True)
    return {offset: json.loads(line)["Symbol"]
            for offset, line in zip(offsets, results.stdout.splitlines(), strict=True)}


def demangle(samples):
    symbols = sorted({frame.symbol for stack in samples for frame in stack
                      if frame.symbol.startswith(("_R", "_ZN"))})
    result = subprocess.run(["c++filt", "-s", "rust"], input="\n".join(symbols),
                            capture_output=True, check=True, text=True)
    return {symbol: CRATE_HASH.sub("", resolved)
            for symbol, resolved in zip(symbols, result.stdout.splitlines(), strict=True)}


def rust_phase(symbol):
    if re.search(r"\b(?:_int_|__libc_|__rust_|__rdl_)?(?:malloc|free|cfree|realloc|calloc)\b",
                 symbol) or "::limit::" in symbol or "::block::" in symbol:
        return "Accounting and charges"
    if re.search(r"\b(?:__)?mem(?:cpy|move|set|pcpy)\w*|::copy_from_slice", symbol):
        return "Copies"
    if re.search(r"hirsel_core_(?:alloc|free|charge|limit|region|retain|release|step)", symbol):
        return "Accounting and charges"
    if re.search(r"poll_ready|runtime::(?:io|scheduler)|scheduled_io|mio::sys::|"
                 r"hirsel_(?:stack|core_rt::(?:tasks|os|wait|net))|"
                 r"clock_gettime|epoll_wait|::poll_read_buf|::read_buf::|"
                 r"__(?:libc_)?recv", symbol):
        return "Scheduler and reactor"
    if re.search(r"httparse::|Http1Transaction>::parse|::parse_headers|"
                 r"::parse_head|::write_method|::uri::|>::read_head", symbol):
        return "Parse"
    if re.search(r"::response::Builder|::service::|http_b1_hyper::main.*closure#0.*closure#0",
                 symbol):
        return "Handler"
    if re.search(r"Http1Transaction>::encode|>::(?:write_head|poll_flush|poll_write|write_vectored)|"
                 r"::h1::encode::|::write_all::|__send|__GI___writev", symbol):
        return "Response write"
    return None


def hirsel_phase(filename, function, line):
    if filename == "http1.hirsel":
        return "Response write" if function in {"write_decimal"} else "Parse"
    if filename in {"router.hirsel", "wsock.hirsel"}:
        return "Route"
    if filename != "http.hirsel":
        return None
    if function in {"segments_of", "segment", "hex", "source_for_head", "change_source"}:
        return "Route"
    if function == "receive_head":
        return "Copies" if "list.append" in line else "Scheduler and reactor"
    if function in {"receive", "after_ms"} or "host.mono_ns" in line:
        return "Scheduler and reactor"
    if "list.append" in line or "discard_prefix" in line or "list.clear" in line:
        return "Copies"
    if "segments_of(" in line or "req = Request(" in line:
        return "Route"
    if function in {"b1_handler", "header", "response"} or "handler(req" in line:
        return "Handler"
    if "bound bytes" in line:
        return "Accounting and charges"
    if function in {"read_head", "framing", "request_policy", "authority", "target",
                    "decoder", "drain", "decode", "parse_authority", "name_is",
                    "equal_authorities", "port"}:
        return "Parse"
    if function in {"write_unit", "write_succeeded", "send_response", "response_head",
                    "append_date", "date_field", "two_digits", "four_digits", "padded", "has_body",
                    "status_line"}:
        return "Response write"
    return None


def phases(profile, binary, http_source=None):
    samples = read_profile(profile)
    symbols = demangle(samples)
    source_lines = http_source.read_text().splitlines() if http_source else []
    locations = resolve_functions(samples, binary)
    binary = str(binary.resolve())
    totals = Counter()
    functions = Counter()
    skid = 0
    for stack, period in samples.items():
        if stack[0].pc >> 63:
            skid += period
        phase = None
        detail = "[unknown]"
        for frame in stack:
            if frame.pc >> 63:
                continue
            name = symbols.get(frame.symbol, frame.symbol)
            detail = name
            if frame.binary == binary:
                for location in locations.get(frame.offset, ()):
                    filename = Path(location["StartFileName"]).name
                    line = location["Line"]
                    start = location["StartLine"]
                    hirsel_function = ""
                    text = ""
                    if filename == "http.hirsel" and 1 <= start <= len(source_lines):
                        match = FUNCTION.search(source_lines[start - 1])
                        hirsel_function = match[1] if match else ""
                        text = source_lines[line - 1] if 1 <= line <= len(source_lines) else ""
                    phase = hirsel_phase(filename, hirsel_function, text)
                    if phase:
                        detail = f"{filename}:{line} {hirsel_function}"
                        break
                    if not source_lines:
                        phase = rust_phase(CRATE_HASH.sub("", location["FunctionName"]))
                        if phase:
                            detail = CRATE_HASH.sub("", location["FunctionName"])
                            break
            phase = phase or rust_phase(name)
            if phase:
                break
        totals[phase or "Unattributed"] += period
        functions[phase or "Unattributed", detail] += period
    total = sum(totals.values())
    return ({key: totals[key] / total for key in PHASES},
            [(phase, name, weight / total) for (phase, name), weight in functions.most_common(40)],
            skid / total)


def ratios(results):
    print("| Phase | Hirsel share | Hirsel / Tokio+httparse | Hirsel / Hyper |")
    print("| --- | ---: | ---: | ---: |")

    def ratio(left, right):
        return f"{left / right:.2f}×" if right else "—"

    for phase in (*PHASES, "Total"):
        hirsel, tokio, hyper = (results[name] for name in ("hirsel", "tokio", "hyper"))
        hi, to, hy = (cost * (1 if phase == "Total" else shares[phase])
                      for cost, shares in (hirsel, tokio, hyper))
        print(f"| {phase} | {hi / hirsel[0]:.1%} | {ratio(hi, to)} | {ratio(hi, hy)} |")
