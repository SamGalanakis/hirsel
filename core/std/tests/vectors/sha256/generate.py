from pathlib import Path
from runpy import run_path

here = Path(__file__).parent
generate = run_path(str(here.parent / 'sha_cavp.py'))['generate']

generate(here, 'sha256', [8, 440, 448, 504, 512], [1304, 9224])
