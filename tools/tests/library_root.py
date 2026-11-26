"""A scratch library root for building workspace members with edited sources."""
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parents[2]


def library_root(work, members, replacements):
    """Return work/core: links to the checkout's core and std, and a core/programs workspace
    of copies of `members` (a package's files must lie inside it). `replacements` maps a
    path under programs, such as 'runner/runner.hirsel', to its replacement text."""
    libraries = work / 'core'
    programs = libraries / 'programs'
    programs.mkdir(parents=True)
    for name in ('core', 'std'):
        (libraries / name).symlink_to(ROOT / 'core' / name)
    (programs / 'hirsel.toml').write_text('[workspace]\nmembers = [' + ', '.join(f'"{m}"' for m in members) + ']\n')
    for member in members:
        shutil.copytree(ROOT / 'core/programs' / member, programs / member,
                        ignore=shutil.ignore_patterns('.hirsel-*'))
    for path, text in replacements.items():
        (programs / path).write_text(text)
    return libraries
