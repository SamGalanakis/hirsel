"""Translate csv-spectrum's paired CSV and JSON files into Hirsel test rows."""
import json
from pathlib import Path


def quoted(s):
    return json.dumps(s, ensure_ascii=False)


def main():
    here = Path(__file__).parent
    rows = []
    strict = None
    for source in sorted((here / 'csvs').glob('*.csv')):
        raw = source.read_bytes()
        if source.stem == 'location_coordinates':
            strict = raw.decode('utf-8'), raw.index(b'"')
            continue
        expected = json.loads((here / 'json' / (source.stem + '.json')).read_bytes())
        header = list(expected[0])
        records = [header, *[[record[key] for key in header] for record in expected]]
        if any('␟' in field or '␞' in field for record in records for field in record):
            raise ValueError(f'reserved fixture separator in {source}')
        text = ''.join(''.join(field + '␟' for field in record) + '␞' for record in records)
        rows.append((source.stem, raw.decode('utf-8'), text))
    if not rows:
        raise ValueError('no csv-spectrum fixtures')
    print(f'fn csv_spectrum_data() -> Inline[(String, String, String), {len(rows)}] {{')
    print('    [')
    for row in rows:
        print('        (' + ', '.join(map(quoted, row)) + '),')
    print('    ]')
    print('}')
    if strict is None:
        raise ValueError('missing location_coordinates.csv')
    print(f'fn csv_spectrum_bare_quote() -> (String, u64) {{ ({quoted(strict[0])}, {strict[1]}) }}')


if __name__ == '__main__':
    main()
