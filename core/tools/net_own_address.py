#!/usr/bin/env python3
"""NET-18 `own_address_denied`, native half (docs/spec/net.md).

Runs in a network namespace the driver makes with `sudo -n ip`, so it needs a
host where that works (a Boat VM; the build host refuses it). One program run
walks the phases, waiting on stdin while the driver changes the namespace:

1. A public-only connect to 192.31.196.1:7000, not yet assigned: issued.
2. The driver adds 192.31.196.1/32 to a dummy interface. A public-only
   connect is Denied and `own_address` reads 1. With the grant's `Any` rule
   covering that address and port, the connect is issued.
3. The driver deletes the address. A public-only connect is issued and
   `own_address` still reads 1.

Example: net_own_address.py --hirsel toolchain/hirsel --backend llvm
"""
import argparse
import os
from pathlib import Path
import subprocess
import sys
import tempfile

PROGRAM = """
fn issued(take attempt: Result[Socket, NetError]) -> bool {
    match take attempt {
        Ok(socket) => {
            let _ = take socket
            true
        },
        Error(Denied) => false,
        Error(_) => true,
    }
}

fn wait(input: Stdin) -> bool waits {
    let mut byte: Bytes = list.with_capacity(1)
    match stdin.read(input, mut byte, Never) {
        Ok(1) => true,
        _ => false,
    }
}

pub fn main(n: Net, input: Stdin, out: Stdout) -> Result[(), Exit] waits {
    // 192.31.196.1 (public class).
    let own = Addr(ip: V4(3223307265u32), port: 7000)
    let mut rules: List[Rule] = list.new()
    list.push(mut rules, Rule(dir: Connect, prefix: V4(0), bits: 0, ports: (1u16, 65535u16), reach: Public))
    let public = result.unwrap(net.narrow(n, rules))
    if !issued(host.connect(public, own, Never)) {
        return Error(Exit(status: 31))
    }
    result.unwrap(stdout.write(out, "added?\\n"))
    if !wait(input) {
        return Error(Exit(status: 30))
    }
    if issued(host.connect(public, own, Never)) || net.denials(n).own_address != 1 {
        return Error(Exit(status: 32))
    }
    if !issued(host.connect(n, own, Never)) {
        return Error(Exit(status: 33))
    }
    result.unwrap(stdout.write(out, "deleted?\\n"))
    if !wait(input) {
        return Error(Exit(status: 30))
    }
    if !issued(host.connect(public, own, Never)) {
        return Error(Exit(status: 34))
    }
    let counts = net.denials(n)
    if counts.own_address != 1 || counts.not_public != 0 {
        return Error(Exit(status: 35))
    }
    Ok(())
}
"""

GRANT = 'Net=Connect 0.0.0.0/0 1-65535 public;Connect 192.31.196.1/32 7000-7000'


def ip(namespace, *args):
    subprocess.run(['sudo', '-n', 'ip', '-n', namespace, *args], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--hirsel', type=Path, required=True)
    parser.add_argument('--backend', choices=['cranelift', 'llvm'], required=True)
    args = parser.parse_args()
    hirsel = args.hirsel.resolve()
    namespace = f'hirsel-own-{os.getpid()}'
    with tempfile.TemporaryDirectory() as scratch:
        app = Path(scratch) / 'app'
        app.mkdir()
        (app / 'main.hirsel').write_text(PROGRAM)
        os.chmod(scratch, 0o755)
        subprocess.run(['sudo', '-n', 'ip', 'netns', 'add', namespace], check=True)
        try:
            ip(namespace, 'link', 'set', 'lo', 'up')
            ip(namespace, 'link', 'add', 'own0', 'type', 'dummy')
            ip(namespace, 'link', 'set', 'own0', 'up')
            user = ['setpriv', f'--reuid={os.getuid()}', f'--regid={os.getgid()}', '--init-groups', '--',
                    'env', f'HOME={os.environ["HOME"]}', f'PATH={os.environ["PATH"]}', f'TMPDIR={scratch}']
            command = ['sudo', '-n', 'ip', 'netns', 'exec', namespace, *user, str(hirsel), 'run', str(app),
                       '--backend', args.backend, '--store', str(Path(scratch) / 'store'),
                       '--grant', GRANT, '--grant', 'Stdin', '--grant', 'Stdout']
            child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
            for prompt, change in (('added?', 'add'), ('deleted?', 'del')):
                line = child.stdout.readline().strip()
                if line != prompt:
                    child.kill()
                    child.wait()
                    sys.exit(f'{args.backend}: expected {prompt!r}, got {line!r}')
                ip(namespace, 'addr', change, '192.31.196.1/32', 'dev', 'own0')
                child.stdin.write('x')
                child.stdin.flush()
            child.stdin.close()
            status = child.wait(timeout=300)
            print(f'{args.backend} own_address_denied: {"PASS" if status == 0 else f"FAIL exit {status}"}')
            sys.exit(status)
        finally:
            subprocess.run(['sudo', '-n', 'ip', 'netns', 'del', namespace], check=False)


if __name__ == '__main__':
    main()
