"""Witness WRITEV byte counts and completions for this fixture on Linux >= 6.13."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import select
import signal


class Ring:
    def __init__(self, root, stack):
        self.root = root
        self.proc = None
        self.lines = root / 'ring.log'
        stack.callback(self.stop)

    def start(self, pid):
        # The file write_iter receives the actual kernel iov_iter byte count.
        # Match submission and completion by request, and file writes by submitter.
        fs = subprocess.check_output(['findmnt', '-n', '-o', 'FSTYPE', self.root / 'mnt'], text=True).strip()
        fn = 'xfs_file_write_iter' if fs == 'xfs' else 'ext4_file_write_iter'
        script = '''
BEGIN { printf("READY\\n"); }
tracepoint:io_uring:io_uring_submit_req /pid == PID && args.opcode == 2/ {
    @op[args.req] = 1; @current[tid] = args.req;
}
kprobe:FS_WRITE /@current[tid]/ {
    $iocb = (struct kiocb *)arg0;
    $iter = (struct iov_iter *)arg1;
    printf("W %p %s %lu\\n", @current[tid], str($iocb->ki_filp->f_path.dentry->d_name.name), $iter->count);
}
tracepoint:io_uring:io_uring_complete /@op[args.req]/ {
    printf("C %p %d\\n", args.req, args.res);
    delete(@op[args.req]); delete(@current[tid]);
}
END { clear(@op); clear(@current); }
'''.replace('PID', str(pid)).replace('FS_WRITE', fn)
        source = self.root / 'ring.bt'
        source.write_text(script)
        with self.lines.open('w') as out:
            self.proc = subprocess.Popen(['bpftrace', '-q', '-B', 'line', source], stdout=out)
        import time
        for _ in range(100):
            if 'READY' in self.lines.read_text():
                return
            if self.proc.poll() is not None:
                break
            time.sleep(0.1)
        raise RuntimeError('private ring observer failed to attach')

    def stop(self):
        if self.proc and self.proc.poll() is None:
            name = subprocess.check_output(['ps', '-o', 'comm=', '-p', str(self.proc.pid)], text=True).strip()
            if name != 'bpftrace':
                raise RuntimeError('ring observer PID changed')
            self.proc.send_signal(signal.SIGINT)
            self.proc.wait()

    def witness(self):
        self.stop()
        pending, short = {}, {}
        for line in self.lines.read_text().splitlines():
            row = line.split()
            if row[:1] == ['W'] and len(row) == 4:
                pending[row[1]] = (row[2], int(row[3]))
            elif row[:1] == ['C'] and row[1] in pending:
                file, size = pending.pop(row[1])
                result = int(row[2])
                if not re.fullmatch(r'j-\d+', file):
                    continue
                if 0 < result < size:
                    short[file] = dict(file=file, opcode='WRITEV', submitted=size, positive_cqe=result)
                if result == -28 and file in short:
                    return [short[file] | dict(next_cqe=result)]
        return []
