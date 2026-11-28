#!/usr/bin/env python3
"""Value models for the ownership soundness generator: SEM-1 integers and `std.data` documents.

`arith_stmt` and `data_stmt` build statements whose required result is computed here: integer
operators and conversions from SEM-1, and document decoding from the data contract (one defect
or one tightened limit per document, so exactly one refusal applies).
"""
from ownership_soundness_features import (
    CAUSE, FAIL_BASE, MASK, NONE_VALUE, PRIME, check, log_order, mk_list, sub,
)

INT_TYPES = ('u8', 'u16', 'u32', 'u64', 'i8', 'i16', 'i32', 'i64')
OVERFLOW = ('fail', CAUSE['Overflow'])
DIVISION = ('fail', CAUSE['Division'])


def width(ty: str) -> int:
    return int(ty[1:])


def signed(ty: str) -> bool:
    return ty[0] == 'i'


def low(ty: str) -> int:
    return -(1 << (width(ty) - 1)) if signed(ty) else 0


def high(ty: str) -> int:
    return (1 << (width(ty) - signed(ty))) - 1


def fits(ty: str, value: int) -> bool:
    return low(ty) <= value <= high(ty)


def wrap(ty: str, value: int) -> int:
    value &= (1 << width(ty)) - 1
    return value - (1 << width(ty)) if signed(ty) and value > high(ty) else value


def literal(ty: str, value: int) -> str:
    return f'{value}{ty}'


def ok(ty: str, value: int):
    return ('ok', value) if fits(ty, value) else OVERFLOW


def truncating_div(a: int, b: int) -> int:
    quotient = abs(a) // abs(b)
    return -quotient if (a < 0) != (b < 0) else quotient


def binary(op: str, ty: str, a: int, b: int):
    """SEM-1: `+ - *` fail with Overflow, `/ %` with Division, shifts by an out-of-range count with Overflow."""
    if op == '+':
        return ok(ty, a + b)
    if op == '-':
        return ok(ty, a - b)
    if op == '*':
        return ok(ty, a * b)
    if op in ('/', '%'):
        if b == 0:
            return DIVISION
        if op == '/':
            return ok(ty, truncating_div(a, b))
        return ('ok', a - b * truncating_div(a, b))
    if b < 0 or b >= width(ty):
        return OVERFLOW
    return ('ok', wrap(ty, a << b) if op == '<<' else a >> b)


def named(op: str, ty: str, a: int, b: int):
    """The named `int` operations; `None` stands for an `Option`'s `None`."""
    plain = {'add': a + b, 'sub': a - b, 'mul': a * b}
    family, _, base = op.partition('_')
    if family == 'wrapping':
        return ('ok', wrap(ty, -a if base == 'neg' else plain[base]))
    if family == 'saturating':
        return ('ok', min(max(plain[base], low(ty)), high(ty)))
    if family == 'checked':
        if base == 'div':
            result = DIVISION if b == 0 else ok(ty, truncating_div(a, b))
        else:
            result = ok(ty, plain[base])
        return result if result[0] == 'ok' else ('ok', None)
    if op == 'abs':
        return ok(ty, abs(a))
    return ok(ty, -a)


def operand(stream, ty: str) -> int:
    edge = [low(ty), low(ty) + 1, 0, 1, 2, high(ty) - 1, high(ty), high(ty) // 2, 7]
    if signed(ty):
        edge += [-1, -2, low(ty) // 2]
    return stream.pick(edge) if stream.below(4) else wrap(ty, stream.raw())


def folded(ty: str, result) -> int:
    kind, value = result
    if kind == 'fail':
        return FAIL_BASE + value
    return NONE_VALUE if value is None else (value & MASK) % PRIME


def value_closure(k: int, ty: str, a: int, b: int, expr: str, option: bool = False) -> str:
    tail = (f'match {expr} {{ Some(r) => bits_{ty}(r), None => {NONE_VALUE}u64 }}' if option else f'bits_{ty}({expr})')
    return (f'let ar{k} = isolate(fn() -> u64 {{ let x{k}: {ty} = code_id({literal(ty, a)}); '
            f'let y{k}: {ty} = {literal(ty, b)}; {tail} }})\n')


def arith_stmt(stream, k: int) -> str:
    choice = stream.below(7)
    ty = stream.pick(INT_TYPES)
    a = operand(stream, ty)
    b = operand(stream, ty)
    open_log = f'let (atx{k}, arx{k}): (Sender[u64], Receiver[u64]) = channel.new(16u64)\n'
    if choice == 0:
        op = stream.pick(('+', '-', '*', '/', '%'))
        return value_closure(k, ty, a, b, f'x{k} {op} y{k}') + check(k, 0, f'fold_iso(ar{k})', folded(ty, binary(op, ty, a, b)))
    if choice == 1:
        op = stream.pick(('<<', '>>', 'neg', 'abs'))
        if op in ('<<', '>>'):
            count = stream.pick([0, 1, width(ty) - 1, width(ty), width(ty) + 1, high(ty)] + ([-1] if signed(ty) else []))
            return value_closure(k, ty, a, count, f'x{k} {op} y{k}') + check(
                k, 0, f'fold_iso(ar{k})', folded(ty, binary(op, ty, a, count)))
        if op == 'neg' and not signed(ty):
            op = 'abs'
        expr = f'-x{k}' if op == 'neg' else f'int.abs(x{k})'
        return value_closure(k, ty, a, b, expr) + check(k, 0, f'fold_iso(ar{k})', folded(ty, named(op, ty, a, b)))
    if choice == 2:
        op = stream.pick(('wrapping_add', 'wrapping_sub', 'wrapping_mul', 'wrapping_neg', 'saturating_add',
                          'saturating_sub', 'saturating_mul', 'checked_add', 'checked_sub', 'checked_mul', 'checked_div'))
        expr = f'int.{op}(x{k})' if op == 'wrapping_neg' else f'int.{op}(x{k}, y{k})'
        return value_closure(k, ty, a, b, expr, option=op.startswith('checked')) + check(
            k, 0, f'fold_iso(ar{k})', folded(ty, named(op, ty, a, b)))
    if choice == 3:
        # Conversions: narrow fails with Overflow, try_narrow gives None, truncate keeps the low bits.
        to = stream.pick(INT_TYPES)
        op = stream.pick(('narrow', 'try_narrow', 'truncate'))
        if op == 'truncate' and width(to) > width(ty):
            op = 'narrow'
        if op == 'truncate':
            result, tail = ('ok', wrap(to, a)), f'let c{k}: {to} = int.truncate(x{k}); bits_{to}(c{k})'
        elif op == 'narrow':
            result, tail = ok(to, a), f'let c{k}: {to} = int.narrow(x{k}); bits_{to}(c{k})'
        else:
            result = ('ok', a if fits(to, a) else None)
            tail = (f'let c{k}: Option[{to}] = int.try_narrow(x{k}); '
                    f'match c{k} {{ Some(r) => bits_{to}(r), None => {NONE_VALUE}u64 }}')
        return (f'let ar{k} = isolate(fn() -> u64 {{ let x{k}: {ty} = code_id({literal(ty, a)}); {tail} }})\n'
                + check(k, 0, f'fold_iso(ar{k})', folded(to, result)))
    if choice == 4:
        # Bounds-checked indexing between owned resources: the failure cleans what was made, newest first.
        n = 1 + stream.below(6)
        xs = mk_list(a & 31, n)
        cells = [1 + stream.below(90) for _ in range(4)]
        steps, values, failure = [], [], None
        for i, (name, size, source) in enumerate(((f'ax{k}', n, xs), (f'aa{k}', 4, cells))):
            op = stream.pick(('-', '/', '%', '+'))
            left = stream.below(8)
            right = stream.below(4)
            steps.append(f'  let al{i}_{k} = result.unwrap(Logged(log: channel.sender(atx{k}), tag: {i + 1}u64))\n'
                         f'  let ai{i}_{k}: u64 = {left}u64\n  let aj{i}_{k}: u64 = {right}u64\n'
                         f'  let av{i}_{k} = {name}[ai{i}_{k} {op} aj{i}_{k}]')
            index = binary(op, 'u64', left, right)
            if failure is None:
                if index[0] == 'fail':
                    failure = (i, index[1])
                elif index[1] >= size:
                    failure = (i, CAUSE['Bounds'])
                else:
                    values.append(source[index[1]])
        want = FAIL_BASE + failure[1] if failure else sum(values) + 1 + 2 + 3
        order = log_order(range(failure[0] + 1, 0, -1)) if failure else 321
        return (open_log + f'let ax{k} = mk_list({a & 31}u64, {n}u64)\n'
                + f'let aa{k}: Inline[u64, 4] = [{", ".join(f"{c}u64" for c in cells)}]\n'
                + f'let ar{k} = isolate(fn() -> u64 {{\n' + '\n'.join(steps) + '\n'
                + f'  let al2_{k} = result.unwrap(Logged(log: channel.sender(atx{k}), tag: 3u64))\n'
                + f'  av0_{k} + av1_{k} + al0_{k}.tag + al1_{k}.tag + al2_{k}.tag\n}})\n'
                + check(k, 0, f'fold_iso(ar{k})', want) + '\n' + check(k, 1, f'drain_log(arx{k})', order))
    small = stream.pick(('u8', 'u16', 'i8', 'i16'))
    if choice == 5:
        # Accumulation up to the boundary: sends issued before the Overflow persist.
        count = 2 + stream.below(5)
        items = [stream.below(high(small) // 2 + 1) for _ in range(count)]
        total, done, result = 0, 0, None
        for item in items:
            result = ok(small, total + item)
            if result[0] == 'fail':
                break
            total, done = result[1], done + 1
        pushes = '\n'.join(f'list.push(mut ab{k}, {literal(small, item)})' for item in items)
        if stream.below(3) == 0:
            return (f'let mut ab{k}: List[{small}] = list.new()\n{pushes}\n'
                    f'let ar{k} = isolate(fn() -> u64 {{ bits_{small}(list.sum(ab{k})) }})\n'
                    + check(k, 0, f'fold_iso(ar{k})', folded(small, result)))
        return (open_log + f'let mut ab{k}: List[{small}] = list.new()\n{pushes}\n'
                + f'let ar{k} = isolate(fn() -> u64 {{\n  let mut total{k}: {small} = {literal(small, 0)}\n'
                + f'  let mut seen{k}: u64 = 0u64\n'
                + f'  for item{k} in ab{k} {{ total{k} = total{k} + item{k}; seen{k} = seen{k} + 1u64; '
                + f'let _ = channel.try_send(atx{k}, seen{k}) }}\n  bits_{small}(total{k})\n}})\n'
                + check(k, 0, f'fold_iso(ar{k})', folded(small, result)) + '\n'
                + check(k, 1, f'drain_log(arx{k})', log_order(range(1, done + 1))))
    # Compound assignment stops at the first operation that fails.
    value, result, lines = a if fits(small, a) else wrap(small, a), None, []
    start = value
    for _ in range(2 + stream.below(3)):
        op = stream.pick(('+', '-', '*', '/', '%'))
        rhs = operand(stream, small)
        lines.append(f'  cur{k} {op}= {literal(small, rhs)}')
        if result is None or result[0] == 'ok':
            result = binary(op, small, value, rhs)
            if result[0] == 'ok':
                value = result[1]
    return (f'let ar{k} = isolate(fn() -> u64 {{\n  let mut cur{k}: {small} = code_id({literal(small, start)})\n'
            + '\n'.join(lines) + f'\n  bits_{small}(cur{k})\n}})\n' + check(k, 0, f'fold_iso(ar{k})', folded(small, result)))


# --- std.data.decode ---

REQUIRED = ('name', 'count', 'small', 'delta', 'on', 'flags', 'labels', 'mode', 'pair')
INVALID = {'unknown field': 101, 'duplicate field': 102, 'missing required field': 103, 'wrong nominal root': 104,
           'data syntax': 105, 'expected scalar literal': 106, 'duplicate map key': 107, 'unexpected data case': 108,
           'wrong positional arity': 109, 'expected bare case': 110, 'expected an option constructor': 111,
           'expected a labelled field': 112}
LIMIT = {'input bytes': 201, 'depth': 202, 'nodes': 203, 'collection length': 204, 'decoded bytes': 205,
         'key comparisons': 206, 'diagnostics': 207}
DECODED = 1000000


def word(stream, lowest: int, most: int) -> str:
    return ''.join('abcdefghijklmnopqrstuvwxyz'[stream.below(26)] for _ in range(lowest + stream.below(most - lowest + 1)))


def document(stream) -> dict:
    labels = {}
    for _ in range(stream.below(4)):
        labels[word(stream, 1, 5)] = stream.below(200)
    mode = stream.pick((('Fast',), ('Slow', stream.below(500)), ('Tuned', stream.below(500), stream.below(200))))
    doc = {
        'name': word(stream, 0, 8), 'count': stream.below(1000), 'small': stream.below(256),
        'delta': stream.below(2001) - 1000, 'on': stream.below(2) == 1,
        'flags': [stream.below(50) for _ in range(stream.below(5))], 'labels': labels, 'mode': mode,
        'pair': (stream.below(1000), stream.below(256)),
    }
    if stream.below(2):
        doc['opt'] = stream.pick((None, stream.below(1000)))
    if stream.below(2):
        doc['retries'] = stream.below(100)
    return doc


def digest(doc: dict) -> int:
    """Model of the prelude's `cfg_digest`, offset as `cfg_decode` offsets a decoded document."""
    mode = doc['mode']
    total = len(doc['name']) + doc['count'] + doc['small'] + doc.get('retries', 3)
    total += 1000 * doc['on'] + 500 * (doc['delta'] < 0) + sum(doc['flags'])
    total += sum(len(key) * 10 + value for key, value in doc['labels'].items())
    total += {'Fast': 1, 'Slow': 2, 'Tuned': 3}[mode[0]] + sum(mode[1:])
    total += sum(doc['pair'])
    return total + (7 if doc.get('opt') is None else doc['opt']) + DECODED


def field_text(key: str, value) -> str:
    if key == 'name':
        return f'"{value}"'
    if key == 'on':
        return 'true' if value else 'false'
    if key == 'flags':
        return '[' + ', '.join(str(flag) for flag in value) + ']'
    if key == 'labels':
        return '[' + ', '.join(f'("{name}", {number})' for name, number in value.items()) + ']'
    if key == 'mode':
        if value[0] == 'Tuned':
            return f'Tuned(level: {value[1]}, bias: {value[2]})'
        return value[0] if len(value) == 1 else f'{value[0]}({value[1]})'
    if key == 'pair':
        return f'({value[0]}, {value[1]})'
    if key == 'opt':
        return 'None' if value is None else f'Some({value})'
    return str(value)


def render(stream, doc: dict, replace: dict | None = None, extra: str = '', root: str = 'cfg.CfgDoc') -> str:
    """Document text with its fields in a drawn order; `replace` swaps one field's text."""
    keys = list(doc)
    for i in range(len(keys) - 1, 0, -1):
        j = stream.below(i + 1)
        keys[i], keys[j] = keys[j], keys[i]
    fields = [f'{key}: {(replace or {}).get(key) or field_text(key, doc[key])}' for key in keys]
    return f'{root}(' + ', '.join(fields) + extra + ')'


def malformed(stream, doc: dict) -> tuple[str, str]:
    """One defect in an otherwise valid document, and the reason the decoder must give."""
    kind = stream.below(12)
    if kind == 0:
        return render(stream, doc, extra=', bogus: 1'), 'unknown field'
    if kind == 1:
        return render(stream, doc, extra=', count: 1'), 'duplicate field'
    if kind == 2:
        missing = stream.pick(REQUIRED)
        return render(stream, {key: value for key, value in doc.items() if key != missing}), 'missing required field'
    if kind == 3:
        return render(stream, doc, root=stream.pick(('cfg.Other', 'other.CfgDoc'))), 'wrong nominal root'
    if kind == 4:
        return render(stream, doc)[:-1], 'data syntax'
    if kind == 5:
        swap = stream.pick(({'small': '256'}, {'count': '-1'}, {'on': '1'}, {'delta': '2147483648'}, {'name': '7'}))
        return render(stream, doc, replace=swap), 'expected scalar literal'
    if kind == 6:
        return render(stream, doc, replace={'labels': '[("dup", 1), ("x", 2), ("dup", 3)]'}), 'duplicate map key'
    if kind == 7:
        return render(stream, doc, replace={'mode': stream.pick(('Turbo', 'Turbo(1)'))}), 'unexpected data case'
    if kind == 8:
        swap = stream.pick(({'pair': '(1, 2, 3)'}, {'mode': 'Slow(1, 2)'}, {'mode': 'Slow'}))
        return render(stream, doc, replace=swap), 'wrong positional arity'
    if kind == 9:
        return render(stream, doc, replace={'mode': 'Fast(1)'}), 'expected bare case'
    if kind == 10:
        return render(stream, doc, replace={'opt': 'Maybe(1)'} if 'opt' in doc else None,
                      extra='' if 'opt' in doc else ', opt: Maybe(1)'), 'expected an option constructor'
    return 'cfg.CfgDoc(1, 2)', 'expected a labelled field'


def quoted(text: str) -> str:
    return 'b"' + text.replace('\\', '\\\\').replace('"', '\\"') + '"'


def data_stmt(stream, k: int) -> str:
    choice = stream.below(8)
    doc = document(stream)
    if choice in (0, 1):
        text = render(stream, doc)
        return (f'let dd{k} = cfg_decode({quoted(text)}, cfg_limits(0u64, 0u64))\n' + check(k, 0, f'dd{k}.0', digest(doc))
                + f'\nacc = mix(acc, dd{k}.1)')
    if choice in (2, 3):
        text, reason = malformed(stream, doc)
        quiet = stream.below(4) == 0
        want = LIMIT['diagnostics'] if quiet else INVALID[reason]
        return (f'let dd{k} = cfg_decode({quoted(text)}, cfg_limits({7 if quiet else 0}u64, 0u64))\n'
                + check(k, 0, f'dd{k}.0', want) + f'\nacc = mix(acc, dd{k}.1)')
    text = render(stream, doc)
    if choice == 4:
        # One tightened limit on a valid document.
        knob, value, label = stream.pick((
            (1, len(text) - 1, 'input bytes'), (1, 0, 'input bytes'), (2, 1, 'depth'), (3, 1, 'nodes'),
            (4, stream.below(len(REQUIRED)), 'collection length'), (5, 0, 'decoded bytes'), (6, 0, 'key comparisons'),
            (1, len(text), None), (7, 0, None)))
        return (f'let dd{k} = cfg_decode({quoted(text)}, cfg_limits({knob}u64, {value}u64))\n'
                + check(k, 0, f'dd{k}.0', LIMIT[label] if label else digest(doc)) + f'\nacc = mix(acc, dd{k}.1)')
    if choice == 5:
        # The decoder's admissions come from the current byte limit.
        tight = stream.below(2) == 0
        return sub(
            'let dd${k} = isolate(fn() -> u64 {\n'
            '  let db${k} = bound bytes ${room} { let dr${k} = cfg_decode(${text}, cfg_limits(0u64, 0u64)); dr${k}.0 }\n'
            '  match db${k} { Ok(v) => v, Error(_) => 8000u64 }\n})\n',
            k=k, room=64 if tight else 4194304, text=quoted(text)) + check(
                k, 0, f'fold_iso(dd{k})', FAIL_BASE + CAUSE['Capacity'] if tight else digest(doc))
    if choice == 6:
        # The source row of MEM-30 takes one step per input byte.
        tight = stream.below(2) == 0
        budget = stream.below(len(text)) if tight else 1048576
        return sub(
            'let dd${k} = bound steps ${budget} { let dr${k} = cfg_decode(${text}, cfg_limits(0u64, 0u64)); dr${k}.0 }\n',
            k=k, budget=budget, text=quoted(text)) + check(
                k, 0, f'match dd{k} {{ Ok(v) => v, Error(t) => 7000u64 + t.steps }}', 7000 + budget if tight else digest(doc))
    # A decoded document is an ordinary owned value: it moves into a Held and back.
    return check(k, 0, f'cfg_held({quoted(text)})', digest(doc))


VALUE_BUILDERS = {'arith': arith_stmt, 'data_decode': data_stmt}
