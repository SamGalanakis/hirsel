"""Witness io_uring file-write opcodes and completions for this fixture on Linux >= 6.13."""
import re
import signal
import subprocess
import time

OPCODES = {2: 'WRITEV', 5: 'WRITE_FIXED', 23: 'WRITE'}


def parse_witness(text):
    pending, short = {}, {}
    completed_writes, completed_bytes, opcodes = {}, {}, {}
    for line in text.splitlines():
        row = line.split()
        if row[:1] == ['W'] and len(row) == 5:
            op = OPCODES.get(int(row[2]))
            if op is not None:
                pending[row[1]] = (op, row[3], int(row[4]))
        elif row[:1] == ['C'] and len(row) == 4 and row[1] in pending:
            opcode, file, size = pending.pop(row[1])
            result = int(row[3])
            if not re.fullmatch(r'j-\d+', file):
                continue
            counts = opcodes.setdefault(file, {})
            counts[opcode] = counts.get(opcode, 0) + 1
            if result > 0:
                completed_writes[file] = completed_writes.get(file, 0) + 1
                completed_bytes[file] = completed_bytes.get(file, 0) + result
                if result < size:
                    short[file] = dict(opcode=opcode, submitted=size, positive_cqe=result)
            elif result == -28 and completed_writes.get(file, 0) > 0:
                entry = dict(
                    file=file,
                    opcode=opcode,
                    submitted=size,
                    cqe=result,
                    completed_writes=completed_writes[file],
                    completed_bytes=completed_bytes[file],
                    opcodes=dict(sorted(counts.items())),
                )
                if file in short:
                    entry['short_write'] = short[file]
                    entry['positive_cqe'] = short[file]['positive_cqe']
                    entry['next_cqe'] = result
                return [entry]
    return []


class Ring:
    def __init__(self, root, stack):
        self.root = root
        self.proc = None
        self.lines = root / 'ring.log'
        self.err = root / 'ring.err'
        stack.callback(self.stop)

    def start(self, pid):
        # The file write_iter receives the actual kernel iov_iter byte count.
        # Track WRITEV (2), WRITE_FIXED (5), and WRITE (23) across both inline
        # issue and io-wq worker threads via io_write/io_write_fixed.
        fs = subprocess.check_output(['findmnt', '-n', '-o', 'FSTYPE', self.root / 'mnt'], text=True).strip()
        fn = 'xfs_file_write_iter' if fs == 'xfs' else 'ext4_file_write_iter'
        script = '''
BEGIN { printf("READY\\n"); }
tracepoint:io_uring:io_uring_submit_req /pid == PID && (args.opcode == 2 || args.opcode == 5 || args.opcode == 23)/ {
    @op[(uint64)args.req] = args.opcode;
}
kprobe:io_write, kprobe:io_write_fixed /@op[arg0]/ {
    @current[tid] = arg0;
}
kretprobe:io_write, kretprobe:io_write_fixed /@current[tid]/ {
    delete(@current[tid]);
}
kprobe:FS_WRITE /@current[tid]/ {
    $req = @current[tid];
    $iocb = (struct kiocb *)arg0;
    $iter = (struct iov_iter *)arg1;
    printf("W 0x%lx %u %s %lu\\n", $req, @op[$req], str($iocb->ki_filp->f_path.dentry->d_name.name), $iter->count);
}
tracepoint:io_uring:io_uring_complete /@op[(uint64)args.req]/ {
    printf("C 0x%lx %u %d\\n", (uint64)args.req, @op[(uint64)args.req], args.res);
    delete(@op[(uint64)args.req]);
}
END { clear(@op); clear(@current); }
'''.replace('PID', str(pid)).replace('FS_WRITE', fn)
        source = self.root / 'ring.bt'
        source.write_text(script)
        with self.lines.open('w') as out, self.err.open('w') as err:
            self.proc = subprocess.Popen(['bpftrace', '-q', '-B', 'line', source], stdout=out, stderr=err)
        for _ in range(100):
            if 'READY' in self.lines.read_text():
                return
            if self.proc.poll() is not None:
                break
            time.sleep(0.1)
        detail = self.err.read_text().strip() if self.err.exists() else ''
        raise RuntimeError(f'private ring observer failed to attach: {detail}' if detail
                           else 'private ring observer failed to attach')

    def stop(self):
        if self.proc and self.proc.poll() is None:
            name = subprocess.check_output(['ps', '-o', 'comm=', '-p', str(self.proc.pid)], text=True).strip()
            if name != 'bpftrace':
                raise RuntimeError('ring observer PID changed')
            self.proc.send_signal(signal.SIGINT)
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()

    def witness(self):
        self.stop()
        return parse_witness(self.lines.read_text())
