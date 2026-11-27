"""Loop/dm devices owned by one matrix cell, including crash replay mounts."""
from contextlib import ExitStack
import os
from pathlib import Path
import subprocess
import tempfile
import uuid

from log_writes import cases, entries, replay


def run(*argv, **kwargs):
    return subprocess.run(argv, check=True, stdout=subprocess.PIPE, **kwargs).stdout


def copy(source, dest):
    run('cp', '--reflink=auto', '--sparse=always', source, dest)


class Volume:
    def __init__(self, fs, fault, stack):
        self.root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='hirsel-chaos-')))
        self.fs = 'xfs' if fs == 'xfs' else 'ext4'
        self.options = 'nouuid' if self.fs == 'xfs' else fs
        self.mount = self.root / 'mnt'
        self.mount.mkdir()
        self.store = self.mount / 's'
        self.name = 'hirsel-chaos-' + uuid.uuid4().hex[:16]
        self.device = Path('/dev/mapper') / self.name
        self.image = self.root / 'data'
        with self.image.open('wb') as f:
            f.truncate(384 << 20 if self.fs == 'xfs' else 64 << 20)
        if self.fs == 'xfs':
            run('mkfs.xfs', '-q', '-f', '-K', self.image)
        else:
            run('mkfs.ext4', '-q', '-F', '-m', '0', '-E', 'nodiscard,lazy_itable_init=0,lazy_journal_init=0',
                self.image)
        # A raw sentinel occupies a tail block outside the formatted filesystem.
        with self.image.open('ab') as f:
            f.write(bytes(4096))
        self.blocks = self.image.stat().st_size // 512
        self.loop = self.attach(self.image, stack)
        self.log = self.root / 'writes'
        spec = f'linear {self.loop} 0'
        if fault == 'log-writes':
            copy(self.image, self.root / 'base')
            with self.log.open('wb') as f:
                f.truncate(96 << 20)
            spec = f'log-writes {self.loop} {self.attach(self.log, stack)}'
        elif fault.startswith('dust'):
            spec = f'dust {self.loop} 0 4096'
        run('dmsetup', 'create', self.name, '--table', f'0 {self.blocks} {spec}')
        stack.callback(self.remove)
        self.mounted = False
        self.cold_mount(self.device)
        stack.callback(self.unmount)
        self.store.mkdir()
        self.replay_stack = ExitStack()
        stack.callback(self.close_replay)

    def attach(self, image, stack):
        dev = run('losetup', '--find', '--show', image).decode().strip()
        stack.callback(run, 'losetup', '-d', dev)
        return dev

    def cold_mount(self, dev):
        run('mount', '-t', self.fs, '-o', self.options, dev, self.mount)
        self.mounted = True

    def unmount(self):
        if self.mounted:
            run('umount', self.mount)
            self.mounted = False

    def remove(self):
        if self.device.exists():
            run('dmsetup', 'remove', '--retry', self.name)

    def switch(self, table):
        run('dmsetup', 'suspend', '--noflush', '--nolockfs', self.name)
        try:
            run('dmsetup', 'load', self.name, '--table', f'0 {self.blocks} {table}')
        finally:
            run('dmsetup', 'resume', self.name)

    def flakey(self, option):
        self.switch(f'flakey {self.loop} 0 0 3600 1 {option}')

    def heal(self):
        self.switch(f'linear {self.loop} 0')

    def mark(self, name):
        run('dmsetup', 'message', self.name, '0', 'mark', name)

    def drop_witness(self):
        offset = str(self.blocks // 8 - 1)
        sample = os.urandom(4096)
        run('dd', f'of={self.device}', 'bs=4096', f'seek={offset}', 'oflag=direct', 'conv=notrunc',
            'status=none', input=sample)
        actual = run('dd', f'if={self.loop}', 'bs=4096', f'skip={offset}', 'iflag=direct', 'count=1',
                     'status=none')
        if actual != bytes(4096):
            raise ValueError('drop_writes failed to drop the sentinel')
        return dict(dm='flakey', feature='drop_writes', completed_bytes=len(sample), underlying_changed=False)

    def finish_log(self):
        self.mark('end-window')
        self.unmount()
        self.remove()
        self.size, self.records = entries(self.log)
        return list(cases(self.records))

    def close_replay(self):
        self.unmount()
        self.replay_stack.close()

    def open_replay(self, case):
        self.close_replay()
        image = self.root / 'replay'
        copy(self.root / 'base', image)
        replay(image, self.log, self.size, self.records, case)
        self.cold_mount(self.attach(image, self.replay_stack))
