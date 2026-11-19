"""Embed unmodified parse-number-fxx-test-data rows for the evaluator."""
import json
from pathlib import Path


def main():
    data = Path(__file__).with_name('more-test-cases.txt').read_text(encoding='ascii')
    if not data or not data.endswith('\n'):
        raise ValueError('expected newline-terminated float fixtures')
    print('fn nigel_parse_number_data() -> String {')
    print('    ' + json.dumps(data))
    print('}')


if __name__ == '__main__':
    main()
