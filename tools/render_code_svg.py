#!/usr/bin/env python3
"""Render Hirsel source snippets to theme-matched SVGs using editors/tree-sitter-hirsel.

Reads the first ```hirsel fenced block from README.md, parses it with the real
Tree-sitter grammar (editors/tree-sitter-hirsel) and queries/highlights.scm, and
renders crisp dark and light SVG code cards to:
  - docs/assets/readme-hero-dark.svg
  - docs/assets/readme-hero-light.svg

Usage: tools/render_code_svg.py [--check]
"""

import ctypes
import html
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GRAMMAR = ROOT / "editors" / "tree-sitter-hirsel"
README = ROOT / "README.md"
ASSETS = ROOT / "docs" / "assets"
DARK_SVG = ASSETS / "readme-hero-dark.svg"
LIGHT_SVG = ASSETS / "readme-hero-light.svg"

FENCE = re.compile(r"^[ \t]*(`{3,}|~{3,})([^\r\n]*)$")

# Tree-sitter highlight capture -> semantic token class.
# Unlisted captures (e.g. local identifiers, module prefixes, parameters) use base ink.
SVG_CLASS = {
    "keyword": "k",
    "keyword.conditional": "k",
    "keyword.repeat": "k",
    "keyword.return": "k",
    "function": "d",
    "type.definition": "d",
    "function.call": "f",
    "type": "t",
    "type.builtin": "t",
    "constructor": "t",
    "attribute": "t",
    "label": "t",
    "string": "v",
    "character": "v",
    "number": "v",
    "number.float": "v",
    "boolean": "v",
    "operator": "o",
    "punctuation.bracket": "o",
    "punctuation.delimiter": "o",
    "comment": "c",
}

THEMES = {
    "dark": {
        "card_bg": "#161b22",
        "header_bg": "#1c2128",
        "border": "#30363d",
        "divider": "#262c36",
        "accent": "#e5534b",
        "title": "#c9d1d9",
        "badge_bg": "#21262d",
        "badge_border": "#30363d",
        "badge_text": "#8b949e",
        "line_no": "#6e7681",
        "ink": "#e6edf3",
        "k": "#f47067",
        "d": "#79c0ff",
        "f": "#79c0ff",
        "t": "#e3b366",
        "v": "#8ddb8c",
        "o": "#8b949e",
        "c": "#8b949e",
    },
    "light": {
        "card_bg": "#f6f8fa",
        "header_bg": "#eff2f5",
        "border": "#d0d7de",
        "divider": "#d8dee4",
        "accent": "#cb271f",
        "title": "#24292f",
        "badge_bg": "#ffffff",
        "badge_border": "#d0d7de",
        "badge_text": "#57606a",
        "line_no": "#8c959f",
        "ink": "#1f2328",
        "k": "#b9231b",
        "d": "#1d5a9e",
        "f": "#1d5a9e",
        "t": "#8a5017",
        "v": "#3d6423",
        "o": "#656d76",
        "c": "#6e7781",
    },
}

FONT_MONO = (
    "'JetBrains Mono', 'SFMono-Regular', ui-monospace, Consolas, Menlo, "
    "'Liberation Mono', monospace"
)
FONT_SANS = (
    "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif"
)

CARD_WIDTH = 780
HEADER_HEIGHT = 40
TOP_PAD = 20
BOTTOM_PAD = 16
LINE_HEIGHT = 24
FONT_SIZE = 14
CHAR_WIDTH = 8.4
LINE_NO_X = 44
CODE_X = 64


def ensure_binding(script_path: str) -> None:
    try:
        import tree_sitter  # noqa: F401
    except ImportError:
        python = (
            Path.home()
            / ".cache"
            / "hirsel-tree-sitter"
            / f"binding-0.25.2-{sys.implementation.cache_tag}"
            / "bin"
            / "python"
        )
        if not python.is_file() or os.environ.get("SITE_HIGHLIGHT_REEXEC"):
            sys.exit(
                "render_code_svg: py-tree-sitter 0.25.2 missing; "
                "run editors/tree-sitter-hirsel/check-local once"
            )
        os.execve(
            python,
            [str(python), script_path, *sys.argv[1:]],
            dict(os.environ, SITE_HIGHLIGHT_REEXEC="1"),
        )


def load_grammar(temp_dir: str):
    from tree_sitter import Language, Parser, Query

    lib = Path(temp_dir) / "hirsel.so"
    subprocess.run(
        [
            "cc",
            "-shared",
            "-fPIC",
            "-O1",
            "-I",
            str(GRAMMAR / "src"),
            str(GRAMMAR / "src" / "parser.c"),
            str(GRAMMAR / "src" / "scanner.c"),
            "-o",
            str(lib),
        ],
        check=True,
    )
    grammar = ctypes.CDLL(str(lib))
    entry = grammar.tree_sitter_hirsel
    entry.restype = ctypes.c_void_p
    capsule = ctypes.pythonapi.PyCapsule_New
    capsule.argtypes = (ctypes.c_void_p, ctypes.c_char_p, ctypes.c_void_p)
    capsule.restype = ctypes.py_object
    language = Language(capsule(entry(), b"tree_sitter.Language", None))
    parser = Parser(language)
    query = Query(language, (GRAMMAR / "queries" / "highlights.scm").read_text())
    return grammar, parser, query


def extract_readme_hero(readme_text: str) -> list[str]:
    opening = None
    contents: list[str] | None = None
    for line in readme_text.splitlines():
        match = FENCE.match(line)
        if opening is None:
            if match:
                delimiter, info = match.groups()
                opening = delimiter
                if info.split()[:1] == ["hirsel"]:
                    contents = []
                else:
                    contents = None
        elif match and match[1].startswith(opening) and not match[2].strip():
            if contents is not None:
                return contents
            opening = None
        elif contents is not None:
            contents.append(line)
    raise ValueError("README.md: expected a ```hirsel code block")


def tokenize_lines(
    parser,
    query,
    lines: list[str],
    class_map: dict[str, str],
    label: str = "snippet",
) -> list[list[list]]:
    from tree_sitter import QueryCursor

    errors = []
    for line in lines:
        if not line.isascii():
            errors.append(f"{label}: non-ASCII line {line!r}")
    source = "\n".join(lines).encode()
    tree = parser.parse(source)
    if tree.root_node.has_error:
        errors.append(f"{label}: parse error\n{tree.root_node}")
    if errors:
        raise ValueError("\n".join(errors))

    # One class per node: earliest pattern in queries/highlights.scm wins.
    chosen: dict[tuple[int, int], tuple[int, str]] = {}
    for pattern, caps in QueryCursor(query).matches(tree.root_node):
        for capture, nodes in caps.items():
            for node in nodes:
                key = (node.start_byte, node.end_byte)
                if key not in chosen or pattern < chosen[key][0]:
                    chosen[key] = (pattern, capture)

    starts = [0]
    for line in lines:
        starts.append(starts[-1] + len(line) + 1)

    ranges: list[list[list]] = [[] for _ in lines]
    for (start_b, end_b), (_, capture) in sorted(chosen.items()):
        cls = class_map.get(capture)
        if not cls:
            continue
        row = max(i for i, s in enumerate(starts) if s <= start_b)
        if end_b > starts[row] + len(lines[row]):
            errors.append(f"{label}: capture {capture} spans lines")
            continue
        ranges[row].append([start_b - starts[row], end_b - start_b, cls])

    for row in ranges:
        for prev, cur in zip(row, row[1:]):
            if cur[0] < prev[0] + prev[1]:
                errors.append(f"{label}: overlapping ranges {prev} {cur}")
    if errors:
        raise ValueError("\n".join(errors))
    return ranges


def _fmt_coord(value: float) -> str:
    text = f"{value:.2f}".rstrip("0").rstrip(".")
    return text or "0"


def _render_line_spans(line: str, spans: list[list], indent: int) -> str:
    text = line[indent:]
    if not text:
        return ""
    parts: list[str] = []
    pos = 0
    for start, length, cls in spans:
        rel_start = start - indent
        if rel_start > pos:
            parts.append(html.escape(text[pos:rel_start], quote=False))
        token = text[rel_start : rel_start + length]
        parts.append(f'<tspan class="{cls}">{html.escape(token, quote=False)}</tspan>')
        pos = rel_start + length
    if pos < len(text):
        parts.append(html.escape(text[pos:], quote=False))
    return "".join(parts)


def render_svg(
    lines: list[str],
    ranges: list[list[list]],
    theme_name: str,
    filename: str = "main.hirsel",
    lang_badge: str = "hirsel",
    aria_label: str = "Hirsel code example",
) -> str:
    theme = THEMES[theme_name]
    height = HEADER_HEIGHT + TOP_PAD + len(lines) * LINE_HEIGHT + BOTTOM_PAD
    w = CARD_WIDTH

    out: list[str] = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{height}" '
        f'viewBox="0 0 {w} {height}" role="img" aria-label="{html.escape(aria_label, quote=True)}">',
        f"  <title>{html.escape(filename, quote=False)} &#x2014; {html.escape(lang_badge, quote=False)}</title>",
        "  <style>",
        f"    .title {{ font: 500 12.5px/1 {FONT_MONO}; fill: {theme['title']}; }}",
        f"    .badge {{ font: 500 11px/1 {FONT_SANS}; fill: {theme['badge_text']}; letter-spacing: 0.02em; }}",
        f"    .ln {{ font: 400 12.5px/1 {FONT_MONO}; fill: {theme['line_no']}; }}",
        f"    .code {{ font: 400 {FONT_SIZE}px/1 {FONT_MONO}; fill: {theme['ink']}; }}",
        f"    .k {{ fill: {theme['k']}; font-weight: 600; }}",
        f"    .d {{ fill: {theme['d']}; font-weight: 600; }}",
        f"    .f {{ fill: {theme['f']}; }}",
        f"    .t {{ fill: {theme['t']}; }}",
        f"    .v {{ fill: {theme['v']}; }}",
        f"    .o {{ fill: {theme['o']}; }}",
        f"    .c {{ fill: {theme['c']}; font-style: italic; }}",
        "  </style>",
        f'  <rect x="0.5" y="0.5" width="{w - 1}" height="{height - 1}" rx="10" ry="10" fill="{theme["card_bg"]}"/>',
        f'  <path d="M1,10 A9,9 0 0 1 10,1 H{w - 10} A9,9 0 0 1 {w - 1},10 V{HEADER_HEIGHT} H1 Z" fill="{theme["header_bg"]}"/>',
        f'  <line x1="1" y1="{HEADER_HEIGHT}.5" x2="{w - 1}" y2="{HEADER_HEIGHT}.5" stroke="{theme["divider"]}" stroke-width="1"/>',
        f'  <circle cx="22" cy="20.5" r="4" fill="{theme["accent"]}"/>',
        f'  <text x="35" y="25" class="title">{html.escape(filename, quote=False)}</text>',
        f'  <rect x="{w - 72}" y="10.5" width="54" height="20" rx="10" fill="{theme["badge_bg"]}" stroke="{theme["badge_border"]}" stroke-width="1"/>',
        f'  <text x="{w - 45}" y="24" text-anchor="middle" class="badge">{html.escape(lang_badge, quote=False)}</text>',
    ]

    first_baseline = HEADER_HEIGHT + TOP_PAD + FONT_SIZE
    for idx, (line, spans) in enumerate(zip(lines, ranges)):
        y = first_baseline + idx * LINE_HEIGHT
        out.append(f'  <text x="{LINE_NO_X}" y="{y}" text-anchor="end" class="ln">{idx + 1}</text>')
        indent = len(line) - len(line.lstrip(" "))
        rendered = _render_line_spans(line, spans, indent)
        if rendered:
            x = _fmt_coord(CODE_X + indent * CHAR_WIDTH)
            out.append(f'  <text x="{x}" y="{y}" class="code" xml:space="preserve">{rendered}</text>')

    out.append(
        f'  <rect x="0.5" y="0.5" width="{w - 1}" height="{height - 1}" rx="10" ry="10" '
        f'fill="none" stroke="{theme["border"]}" stroke-width="1"/>'
    )
    out.append("</svg>\n")
    return "\n".join(out)


def readme_hero_outputs(parser, query) -> dict[Path, str]:
    lines = extract_readme_hero(README.read_text(encoding="utf-8"))
    ranges = tokenize_lines(parser, query, lines, SVG_CLASS, label="README.md")
    aria = (
        "Hirsel hello world example showing explicit Stdout capability, "
        "mutable List and String, and waits effect"
    )
    return {
        DARK_SVG: render_svg(lines, ranges, "dark", aria_label=aria),
        LIGHT_SVG: render_svg(lines, ranges, "light", aria_label=aria),
    }


def sync_readme_svgs(parser, query, check: bool) -> list[Path]:
    want = readme_hero_outputs(parser, query)
    stale = [
        path
        for path, content in want.items()
        if not path.is_file() or path.read_text(encoding="utf-8") != content
    ]
    if check:
        return stale
    for path in stale:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(want[path], encoding="utf-8")
    return []


def main() -> None:
    check = sys.argv[1:] == ["--check"]
    if sys.argv[1:] not in ([], ["--check"]):
        sys.exit("usage: render_code_svg.py [--check]")
    ensure_binding(__file__)
    with tempfile.TemporaryDirectory(prefix="render-code-svg-") as temp:
        _grammar, parser, query = load_grammar(temp)
        try:
            stale = sync_readme_svgs(parser, query, check=check)
        except ValueError as error:
            sys.exit(f"render_code_svg:\n{error}")
    if stale:
        rel = ", ".join(str(p.relative_to(ROOT)) for p in stale)
        sys.exit(f"render_code_svg: stale SVG(s): {rel}; run tools/site_highlight.py")


if __name__ == "__main__":
    main()
