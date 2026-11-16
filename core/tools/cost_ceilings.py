"""Land owns compact ceiling documents and their decreasing measurement cells."""
import json

DIRECTORY = 'crates/hirsel-core-front/'
FILES = {target: DIRECTORY + name for target, name in (
    ('core-check-cost', 'cost-ceilings.json'), ('core-front-cost', 'front-cost-ceilings.json'))}
POLICY = ('Land lowers measured ceilings only, with the existing 2% margin. '
          'New cells start at their measured margin. Regressions are reported per ticket. '
          'Request an increase with a reviewed tools/cost-ceiling-requests/<lane>.json '
          'containing file, cell, previous, value and reason. The fixed budget stays 60000.')


def compact(target, document):
    if target == 'core-front-cost':
        corpora = {name: {key: row.get(key, {}) for key in ('instructions_per_token', 'image_per_token')}
                   for name, row in document['corpora'].items()}
        return dict(policy=POLICY, suite_targets_instructions=document['suite_targets_instructions'], corpora=corpora)
    corpora = {}
    for name, row in document['corpora'].items():
        if name == 'product':
            corpora[name] = {key: row[key] for key in ('fixed_std_instructions', 'fixed_core_instructions') if key in row}
            corpora[name]['entries'] = {entry: dict(marginal_per_line=value['marginal_per_line'])
                                       for entry, value in row.get('entries', {}).items()}
        else:
            corpora[name] = {key: value for key, value in row.items() if key.endswith('_per_line')}
    return dict(policy=POLICY, finished_budget_per_line=document['finished_budget_per_line'], corpora=corpora)


def lower(saved, measured, path=()):
    """Existing cells take min; new cells use the measurement's margin."""
    changes = []
    for key, value in measured.items():
        cell = (*path, key)
        if isinstance(value, dict):
            changes.extend(lower(saved.setdefault(key, {}), value, cell))
        elif key not in saved or value < saved[key]:
            previous = saved.get(key)
            saved[key] = value
            changes.append(dict(cell=list(cell), previous=previous, value=value))
    return changes


def ratchet(root, reports):
    """Apply measurements and explicit reviewed requests to the merged tree."""
    documents = {path: compact(target, json.loads((root / path).read_text()))
                 for target, path in FILES.items() if (root / path).is_file()}
    changes, requests = [], sorted((root / 'tools/cost-ceiling-requests').glob('*.json'))
    for request in requests:
        row = json.loads(request.read_text())
        path, cell = row['file'], row['cell']
        if path not in documents or not cell or cell[0] != 'corpora' or not row['reason'].strip():
            raise ValueError(f'invalid cost ceiling request: {request}')
        table = documents[path]
        for key in cell[:-1]:
            table = table[key]
        previous = table[cell[-1]]
        value = row['value']
        if previous != row['previous'] or type(value) not in (int, float) or not previous < value < float('inf'):
            raise ValueError(f'stale or invalid cost ceiling increase: {request}')
        table[cell[-1]] = value
        changes.append(dict(file=path, cell=cell, previous=previous, value=value,
                            reason=row['reason'], request=str(request.relative_to(root))))
    for report in reports:
        path = FILES[report['target']]
        for change in lower(documents[path]['corpora'], report['ceilings'], ('corpora',)):
            changes.append(dict(file=path, **change))
    written = []
    for path, document in documents.items():
        output = json.dumps(document, indent=2) + '\n'
        if (root / path).read_text() != output:
            (root / path).write_text(output)
            written.append(path)
    for request in requests:
        request.unlink()
        written.append(str(request.relative_to(root)))
    return written, changes
