"""Emit Hirsel test tables from the pinned JSONTestSuite test_parsing files."""

from pathlib import Path


FINITE = {
    'i_number_double_huge_neg_exp',
    'i_number_real_underflow',
    'i_number_too_big_neg_int',
    'i_number_too_big_pos_int',
    'i_number_very_big_negative_int',
    'i_structure_500_nested_arrays',
}
OVERFLOW = {
    'i_number_huge_exp',
    'i_number_neg_int_huge_exp',
    'i_number_pos_double_huge_exp',
    'i_number_real_neg_overflow',
    'i_number_real_pos_overflow',
}
DEEP = {
    'i_structure_500_nested_arrays',
    'n_structure_100000_opening_arrays',
    'n_structure_open_array_object',
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
    rows = []
    for path in sorted(Path(__file__).with_name('test_parsing').glob('*.json')):
        name = path.stem
        if name in DEEP:
            continue  # Native Stack run stops. See README.md for the affected cases.
        data = path.read_bytes()
        rows.append(f'{expect(name, data)}{name} {data.hex()}')
    if not rows:
        raise ValueError('JSONTestSuite input directory is empty')
    print('fn json_test_suite_data() -> String {')
    print('    "' + '\\n'.join(rows) + '\\n"')
    print('}')


if __name__ == '__main__':
    main()
