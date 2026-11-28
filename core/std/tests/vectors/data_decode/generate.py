"""Supply the same declarations and document bytes as the host schema tests."""
import json
import re
from pathlib import Path

schema = Path(__file__).resolve().parents[5] / 'crates' / 'hirsel-config-schema'
for module in ('grants', 'pkg_v1', 'user_v1', 'host_v1'):
    print((schema / 'schemas' / f'{module}.hirsel').read_text())
# The separate app has its own Grant declaration in the same test compilation unit.
print(re.sub(r'\bGrant\b', 'SiteGrant', (schema / 'fixtures/orders_v1.hirsel').read_text()))
for name, file in (('workspace', 'hirsel.hcfg'), ('package', 'app/hirsel.hcfg'),
                   ('config', 'config.hcfg'), ('group', 'group.hcfg'), ('site', 'site.hcfg')):
    source = (schema / 'fixtures' / file).read_bytes().decode('ascii')
    print(f'fn {name}_bytes() -> Bytes {{ b{json.dumps(source)} }}')
