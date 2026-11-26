import {open, mkdir, readFile, readlink, rename, symlink, unlink} from 'node:fs/promises';
import {basename, join, resolve} from 'node:path';
import {setTimeout as delay} from 'node:timers/promises';
import {CorruptState, type Storage, type Installation, type Lease} from './client.js';
import {encodeState, decodeState} from './persistence.js';

export class NodeStorage implements Storage {
  private directory: string;
  private leaseGenerations = new Map<string, number>();

  constructor(directory: string) { this.directory = resolve(directory); }

  async lock(name: string, wait: boolean): Promise<Lease | null> {
    const base = join(this.directory, name === 'install' ? 'install.lease' : name + '.stream.lease');
    const owner = join(this.directory, `.owner-${process.pid}-${crypto.randomUUID()}`);
    let generation = this.leaseGenerations.get(base) ?? 0;
    for (;;) {
      const path = generation === 0 ? base : `${base}.${generation}`;
      // symlink publishes the owner identity and claims the path in one exclusive operation.
      try {
        await symlink(owner, path).catch(async error => {
          if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
          await mkdir(this.directory, {recursive: true, mode: 0o700});
          await symlink(owner, path);
        });
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
        const other = await readOwner(path);
        if (other === null) continue;
        if (ownerExited(other)) {
          if (await readOwner(path) !== other) continue;
          // Leave the dead claim in place. All contenders advance to the same
          // successor without racing to unlink a new owner's claim.
          this.leaseGenerations.set(base, ++generation);
          continue;
        }
        if (!wait) return null;
        await delay(4);
        continue;
      }
      let end!: () => void;
      const lost = new Promise<void>(resolve => { end = resolve; });
      let release: Promise<void> | undefined;
      return {lost, release: () => release ??= unlink(path).then(end)};
    }
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
    const temporary = join(this.directory, `.state-${process.pid}-${crypto.randomUUID()}`);
    const file = await open(temporary, 'wx', 0o600).catch(async error => {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
      await mkdir(this.directory, {recursive: true, mode: 0o700});
      return open(temporary, 'wx', 0o600);
    });
    try {
      try { await file.writeFile(encodeState(state)); await file.sync(); }
      finally { await file.close(); }
      await rename(temporary, join(this.directory, 'state.json'));
    } catch (error) { await unlink(temporary).catch(() => {}); throw error; }
    const directory = await open(this.directory, 'r');
    try { await directory.sync(); } finally { await directory.close(); }
  }
}

async function readOwner(path: string): Promise<string | null> {
  try { return await readlink(path); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw error;
  }
}

function ownerExited(owner: string): boolean {
  const match = /^\.owner-([1-9][0-9]*)-[a-f0-9-]{36}$/.exec(basename(owner));
  const pid = Number(match?.[1]);
  if (!Number.isInteger(pid) || pid > 0x7fffffff) throw new Error('Invalid client lease owner');
  try { process.kill(pid, 0); return false; }
  catch (error) {
    const {code} = error as NodeJS.ErrnoException;
    if (code === 'ESRCH') return true;
    if (code === 'EPERM') return false;
    throw error;
  }
}
