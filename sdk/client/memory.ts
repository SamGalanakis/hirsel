import {copyInstallation, type Installation, type Lease, type Storage} from './persistence.js';

interface LockCell { held: boolean; queue: Array<() => void> }

export class MemoryStorage implements Storage {
  private installation: Installation | null = null;
  private readonly locks = new Map<string, LockCell>();

  async lock(name: string, wait: boolean): Promise<Lease | null> {
    const existing = this.locks.get(name);
    const cell = existing ?? {held: false, queue: []};
    if (!existing) this.locks.set(name, cell);
    if (!cell.held) {
      cell.held = true;
      return this.lease(name, cell);
    }
    if (!wait) return null;
    return new Promise(resolve => cell.queue.push(() => resolve(this.lease(name, cell))));
  }

  private lease(name: string, cell: LockCell): Lease {
    let end!: () => void;
    const lost = new Promise<void>(resolve => { end = resolve; });
    let released = false;
    return {
      lost,
      release: async () => {
        if (released) return;
        released = true;
        end();
        const next = cell.queue.shift();
        if (next) next();
        else {
          cell.held = false;
          this.locks.delete(name);
        }
      },
    };
  }

  async read(): Promise<Installation | null> {
    return this.installation === null ? null : copyInstallation(this.installation);
  }

  async write(state: Installation): Promise<void> {
    this.installation = copyInstallation(state);
  }
}
