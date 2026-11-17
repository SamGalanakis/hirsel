import {open, mkdir, readFile, rename, unlink} from 'node:fs/promises';
import {join} from 'node:path';
import {spawn} from 'node:child_process';
import {CorruptState, type Storage, type Installation, type Lease} from './client.js';
import {encodeState, decodeState} from './persistence.js';

export class NodeStorage implements Storage {
  constructor(private directory: string) {}
  async lock(name: string, wait: boolean): Promise<Lease | null> {
    await mkdir(this.directory, {recursive: true, mode: 0o700});
    // flock(2) locks an open file description. The helper inherits a dup of
    // Node's descriptor, acquires the lock, and exits. Node keeps the same
    // description open for the entire lease, including after helper exit.
    const file = await open(join(this.directory, name === 'install' ? 'lock' : name + '.lock'), 'a', 0o600);
    let diagnostic = '';
    try {
      const code = await new Promise<number | null>((resolve, reject) => {
        const child = spawn('flock', [...(wait ? [] : ['-n']), '-E', '75', '3'], {stdio: ['ignore', 'ignore', 'pipe', file.fd]});
        child.stderr!.setEncoding('utf8');
        child.stderr!.on('data', chunk => { diagnostic = (diagnostic + chunk).slice(-4096); });
        child.once('error', reject); child.once('close', resolve);
      });
      if (code !== 0) {
        await file.close();
        if (!wait && code === 75) return null;
        throw new Error(`flock failed (${code}): ${diagnostic}`);
      }
    } catch (error) { await file.close(); throw error; }
    let released = false, end!: () => void;
    const lost = new Promise<void>(resolve => { end = resolve; });
    return {lost, release: async () => {
      if (released) return;
      released = true; await file.close(); end();
    }};
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
