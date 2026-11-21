"""Embed the hand-written two-span OTLP document for tests/trace.hirsel.

tools/tests/obs_export.py checks the same file against obs_export.py's span builder.
"""
import json
from pathlib import Path


def main():
    text = Path(__file__).with_name('two_spans.json').read_text(encoding='ascii')
    if not text.endswith('\n') or text.count('\n') != 1:
        raise ValueError('two_spans.json must be one line')
    print('fn two_spans_fixture() -> String {')
    print('    ' + json.dumps(text[:-1]))
    print('}')


if __name__ == '__main__':
    main()
