import {open, mkdir, readFile, rename, unlink} from 'node:fs/promises';
import {join} from 'node:path';
import {spawn} from 'node:child_process';
import {CorruptState, type Storage, type Installation, type Lease} from './client.js';
import {encodeState, decodeState} from './persistence.js';

export class NodeStorage implements Storage {
  constructor(private directory: string) {}
  async lock(name: string, wait: boolean): Promise<Lease | null> {
    await mkdir(this.directory, {recursive: true, mode: 0o700});
    // Node has no flock syscall API. util-linux flock owns the descriptor;
    // its child exits on stdin EOF, including when this Node process dies.
    const child = spawn('flock', [...(wait ? [] : ['-n']), '-E', '75', join(this.directory, name === 'install' ? 'lock' : name + '.lock'),
      process.execPath, '--input-type=module', '-e', 'process.stdout.write("locked\\n");process.stdin.resume();'], {stdio: ['pipe', 'pipe', 'pipe']});
    let released = false;
    let ended!: () => void;
    const lost = new Promise<void>(resolve => { ended = resolve; });
    const exit = new Promise<void>(resolve => child.once('close', () => { ended(); resolve(); }));
    return new Promise<Lease | null>((resolve, reject) => {
      let acquired = false, diagnostic = '', output = '';
      child.stderr.setEncoding('utf8');
      child.stderr.on('data', chunk => { diagnostic = (diagnostic + chunk).slice(-4096); });
      child.stdout.setEncoding('utf8');
      child.stdout.on('data', chunk => {
        output += chunk;
        if (!acquired && output.includes('locked\n')) {
          acquired = true;
          resolve({lost, release: async () => {
            if (!released) { released = true; child.stdin.end(); }
            await exit;
          }});
        }
      });
      child.stdin.on('error', () => {});
      child.once('error', reject);
      child.once('close', code => {
        if (!acquired) {
          if (!wait && code === 75) resolve(null);
          else reject(new Error(`flock failed (${code}): ${diagnostic}`));
        }
      });
    });
  }
  async read(): Promise<Installation | null> {
    try { return decodeState(await readFile(join(this.directory, 'state.json'), 'utf8')); }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
      if (error instanceof CorruptState) throw error;
      throw new CorruptState(`unreadable client state: ${String(error)}`);
    }
  }
  async write(state: Installation): Promise<void> {
    await mkdir(this.directory, {recursive: true, mode: 0o700});
    const temporary = join(this.directory, `.state-${process.pid}-${crypto.randomUUID()}`);
    const file = await open(temporary, 'wx', 0o600);
    try {
      await file.writeFile(encodeState(state)); await file.sync();
    } catch (error) { await unlink(temporary).catch(() => {}); throw error; }
    finally { await file.close(); }
    await rename(temporary, join(this.directory, 'state.json'));
    const directory = await open(this.directory, 'r');
    try { await directory.sync(); } finally { await directory.close(); }
  }
}
