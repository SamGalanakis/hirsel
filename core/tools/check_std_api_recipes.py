#!/usr/bin/env python3
"""Compile std API recipes as external consumers and check selected Store headers.

The Markdown recipes are examples for application code, not declarations inside std.
This harness therefore places every usage fence in a small consumer package that depends
on std and checks it with --source-std. Declaration fences are not consumer code; the
Store pagination and follower-service headers that duplicate the package interface are
compared with `hirsel inspect` output.
"""

import argparse
import json
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
API = ROOT / "core" / "std" / "api"
STORE_SPEC = ROOT / "core" / "programs" / "store" / "STORE-SPEC.md"
STORE_PACKAGE = ROOT / "core" / "programs" / "store"


def fences(path):
    lines = path.read_text().splitlines()
    found = []
    inside = False
    start = 0
    body = []
    for number, line in enumerate(lines, 1):
        if line.startswith("```"):
            if not inside and line.strip() == "```hirsel":
                inside = True
                start = number + 1
                body = []
            elif inside and line.strip() == "```":
                found.append((start, "\n".join(body)))
                inside = False
            elif inside:
                body.append(line)
        elif inside:
            body.append(line)
    return found


def first_token(source):
    for line in source.splitlines():
        stripped = line.strip()
        if stripped and not stripped.startswith("//"):
            return stripped.split(None, 1)[0]
    return ""


def recipes():
    result = []
    for path in sorted(API.rglob("*.md")):
        recipe_index = 0
        for line, source in fences(path):
            token = first_token(source)
            if token.startswith("pub") or not token:
                continue
            key = f"{path.relative_to(API)}:{recipe_index}"
            result.append((key, line, source))
            recipe_index += 1
    return result


FRAGMENTS = {
    "config.md:0": (
        "fn start(address: String, port: u16, timeout: u64) {}\n"
        "fn report(message: String) {}\n"
        "fn recipe(e: Env, a: Args) {\n",
        "\n}\n",
    ),
    "dir.md:0": (
        "fn visit(e: std.dir.Entry) {}\n"
        "fn report(problem: std.dir.WalkError) {}\n"
        "fn recipe(d: Dir) waits {\n",
        "\n}\n",
    ),
    "file.md:0": (
        "fn recipe(d: Dir, payload: Bytes) -> Result[(), std.file.ReplaceError] waits {\n",
        "\nOk(())\n}\n",
    ),
    "http_client.md:0": (
        "fn recipe(n: Net, c: Clock, tasks: Tasks, token: Secret) -> Result[(), std.http_client.FetchError] waits {\n",
        "\nOk(())\n}\n",
    ),
    "trace.md:0": (
        "fn recipe(c: Clock, err: Stderr, level: std.trace.Level, span: std.trace.Span, n: i64) -> Result[(), core.WriteError] waits {\n",
        "\nOk(())\n}\n",
    ),
}


def config_main_body(source):
    opening = source.index("{")
    closing = source.rfind("}")
    return source[opening + 1:closing]


def consumer_module(key, source):
    if key in FRAGMENTS:
        prefix, suffix = FRAGMENTS[key]
        return prefix + source + suffix
    if key == "config.md:1" and source.lstrip().startswith("fn main("):
        # The documented application supplies `run`; the empty stderr write gives this
        # consumer wrapper the waits effect that the documented entry honestly declares.
        return (
            "fn run(port: u16, verbose: bool, tags: List[String], level: std.trace.Level, inputs: List[String]) -> Result[(), String] { Ok(()) }\n"
            "fn recipe(e: Env, a: Args, err: Stderr) -> Result[(), String] waits {\n"
            "    let _ = stderr.write(err, \"\")\n"
            + config_main_body(source)
            + "\n}\n"
        )
    token = first_token(source)
    if token in {"type", "fn", "@test"} or token.startswith("type"):
        return source + "\n"
    raise ValueError(f"std API recipe {key} has no consumer context")


def module_name(index):
    return f"recipe_{index}"


def write_consumer(work, found):
    package = work / "consumer"
    package.mkdir()
    entries = ['"main.hirsel" = "main"']
    (package / "main.hirsel").write_text("pub fn main() -> Result[(), u8] { Ok(()) }\n")
    for index, (key, _line, source) in enumerate(found):
        name = module_name(index)
        filename = name + ".hirsel"
        try:
            text = consumer_module(key, source)
        except ValueError as error:
            raise ValueError(f"{error} (fence line {_line})") from None
        (package / filename).write_text(text)
        entries.append(f'"{filename}" = "{name}"')
    manifest = "[package]\nname = \"std_api_recipes\"\nmajor = 1\ncharge = \"charge-v1\"\n\n"
    manifest += "[dependencies]\nstd = { major = 1 }\n\n[sources]\n" + "\n".join(entries) + "\n"
    (package / "hirsel.toml").write_text(manifest)
    return package


def run(command, cwd):
    return subprocess.run(command, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def compile_recipes(compiler, work, found):
    package = write_consumer(work, found)
    checked = run(
        [compiler, "check", str(package), "--libraries", str(ROOT / "core"), "--source-std"],
        cwd=work,
    )
    if checked.returncode:
        output = checked.stdout + checked.stderr
        raise ValueError("std API consumer recipes do not check:\n" + output.strip())


def strip_comment(line):
    return line.split("//", 1)[0].rstrip()


def declarations(source):
    result = []
    current = []
    depth = 0
    for raw in source.splitlines():
        line = strip_comment(raw)
        if not current and not line.lstrip().startswith("pub "):
            continue
        current.append(line.strip())
        depth += line.count("{") - line.count("}")
        if depth <= 0 and current:
            result.append("\n".join(current))
            current = []
            depth = 0
    if current:
        result.append("\n".join(current))
    return result


def normalized(source):
    return " ".join(source.split())


def declaration_key(source):
    compressed = normalized(source)
    if match := re.match(r"pub fn ([A-Za-z_][A-Za-z0-9_]*)", compressed):
        return ("fn", match[1])
    if match := re.match(r"pub (?:opaque )?type ([A-Za-z_][A-Za-z0-9_]*)", compressed):
        return ("type", match[1])
    return None


def store_interfaces(compiler, work):
    package = work / "store-package"
    package.mkdir()
    for source in STORE_PACKAGE.glob("*.hirsel"):
        shutil.copy(source, package / source.name)
    shutil.copy(STORE_PACKAGE / "hirsel.toml", package / "hirsel.toml")
    inspected = run(
        [compiler, "inspect", str(package), "--interface", "--json", "--libraries", str(ROOT / "core")],
        cwd=work,
    )
    if inspected.returncode:
        raise ValueError("cannot inspect the store package:\n" + (inspected.stdout + inspected.stderr).strip())
    interface = json.loads(inspected.stdout)
    for module in interface["modules"]:
        if module["path"] == ["store"]:
            return {("fn" if item["kind"] == "function" else item["kind"], item["name"]): normalized(item["signature"]) for item in module["items"]}
    raise ValueError("store interface has no store module")


def check_store_headers(compiler, work):
    actual = store_interfaces(compiler, work)
    wanted = {
        ("type", "Limits"),
        ("type", "Resume"),
        ("type", "Page"),
        ("type", "Resumed"),
        ("fn", "scan_page"),
        ("fn", "find_page"),
        ("fn", "scan_next"),
        ("fn", "find_next"),
        ("fn", "follower_service"),
    }
    seen = {}
    for line, source in fences(STORE_SPEC):
        for declaration in declarations(source):
            key = declaration_key(declaration)
            if key in wanted and key not in seen:
                seen[key] = (line, normalized(declaration))
    missing = sorted(key for key in wanted if key not in seen)
    errors = []
    if missing:
        errors.append("missing advertised Store declarations: " + ", ".join(f"{kind} {name}" for kind, name in missing))
    for key, (line, advertised) in sorted(seen.items()):
        if key not in actual:
            errors.append(f"core/programs/store/STORE-SPEC.md:{line}: {key[0]} {key[1]} is not in the store interface")
        elif advertised != actual[key]:
            errors.append(
                f"core/programs/store/STORE-SPEC.md:{line}: advertised {key[0]} {key[1]} does not match the store interface\n"
                f"advertised: {advertised}\nactual:     {actual[key]}"
            )
    if errors:
        raise ValueError("\n".join(errors))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--compiler", required=True)
    args = parser.parse_args()
    try:
        found = recipes()
        if len(found) < 9:
            raise ValueError(f"expected at least 9 std API usage recipes, found {len(found)}")
        with tempfile.TemporaryDirectory(prefix="hirsel-std_api_recipes-") as temporary:
            work = pathlib.Path(temporary)
            compile_recipes(args.compiler, work, found)
            check_store_headers(args.compiler, work)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        sys.stderr.write(str(error) + "\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
