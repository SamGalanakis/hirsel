"""Emit Hirsel test tables from the pinned JSONTestSuite test_parsing files."""

from pathlib import Path


FINITE = {
    'i_number_double_huge_neg_exp',
    'i_number_real_underflow',
    'i_number_too_big_neg_int',
    'i_number_too_big_pos_int',
    'i_number_very_big_negative_int',
}
OVERFLOW = {
    'i_number_huge_exp',
    'i_number_neg_int_huge_exp',
    'i_number_pos_double_huge_exp',
    'i_number_real_neg_overflow',
    'i_number_real_pos_overflow',
}
DEEP = {
    'i_structure_500_nested_arrays': (b'[', 500, b']' * 500),
    'n_structure_100000_opening_arrays': (b'[', 100_000, b''),
    'n_structure_open_array_object': (b'[{"":', 50_000, b'\n'),
}


def expect(name, data):
    try:
        data.decode('utf-8')
    except UnicodeDecodeError:
        if name.startswith(('i_', 'n_')):
            return 'U'
        raise
    if name.startswith('y_') or name in FINITE:
        return 'A'
    if name in OVERFLOW:
        return 'N'
    if name.startswith('n_'):
        return 'R'
    if name.startswith(('i_string_', 'i_object_key_')) or name == 'i_structure_UTF-8_BOM_empty_object':
        return 'S'
    raise ValueError(f'missing expected outcome for {name}')


def main():
    # One static string keeps LLVM from constructing hundreds of owned rows.
    # Each row is a tag, filename, space, original bytes in hex, and newline.
    rows, deep = [], []
    for path in sorted(Path(__file__).with_name('test_parsing').glob('*.json')):
        name = path.stem
        data = path.read_bytes()
        if name in DEEP:
            unit, count, tail = DEEP[name]
            if unit * count + tail != data:
                raise ValueError(f'nesting fixture changed: {name}')
            deep.append((name, unit.decode(), count, tail.decode()))
            continue
        rows.append(f'{expect(name, data)}{name} {data.hex()}')
    if not rows:
        raise ValueError('JSONTestSuite input directory is empty')
    print('fn json_test_suite_data() -> String {')
    print('    "' + '\\n'.join(rows) + '\\n"')
    print('}')
    # Use repetition to keep the exact long inputs without giant source tokens.
    if len(deep) != len(DEEP):
        raise ValueError('missing JSONTestSuite nesting input')
    print(f'fn json_test_suite_deep_data() -> Inline[(String, String, u64, String), {len(deep)}] {{')
    print('    [')
    for name, unit, count, tail in deep:
        escaped_unit = unit.replace('"', '\\"')
        escaped_tail = tail.replace('\n', '\\n')
        print(f'        ("{name}", "{escaped_unit}", {count}, "{escaped_tail}"),')
    print('    ]')
    print('}')


if __name__ == '__main__':
    main()
