import {CorruptState, encodeState, decodeState, type Storage, type Installation, type Lease} from './persistence.js';

export interface Socket {
  binaryType: string;
  // Bytes queued and not yet sent; rooms read it to coalesce Set and drop Send.
  readonly bufferedAmount?: number;
  onopen: ((event?: unknown) => void) | null;
  onmessage: ((event: {data: unknown}) => void) | null;
  onclose: ((event: {code: number; reason?: string}) => void) | null;
  onerror: ((event?: unknown) => void) | null;
  send(data: Uint8Array): void; close(code?: number, reason?: string): void;
}
export interface Clock {
  now(): number; random(): number;
  set(fn: () => void, ms: number): unknown; clear(id: unknown): void;
}

export const defaultClock: Clock = {
  now: () => performance.now(), random: () => Math.random(),
  set: (fn, ms) => setTimeout(fn, ms), clear: id => clearTimeout(id as ReturnType<typeof setTimeout>),
};

export function listenForBrowserWake(wake: () => void): (() => void) | undefined {
  if (typeof window === 'undefined' || typeof document === 'undefined') return;
  const page = document;
  const browser = window;
  const onVisibility = () => { if (page.visibilityState === 'visible') wake(); };
  browser.addEventListener('online', wake);
  page.addEventListener('visibilitychange', onVisibility);
  return () => {
    browser.removeEventListener('online', wake);
    page.removeEventListener('visibilitychange', onVisibility);
  };
}

export class BrowserStorage implements Storage {
  private db?: IDBDatabase;
  constructor(private name = 'hirsel') {}
  async lock(name: string, wait: boolean): Promise<Lease | null> {
    let grant!: (lease: Lease | null) => void;
    let fail!: (error: unknown) => void;
    const acquired = new Promise<Lease | null>((resolve, reject) => { grant = resolve; fail = reject; });
    let release!: () => void;
    const held = new Promise<void>(resolve => { release = resolve; });
    let lost!: () => void;
    const ended = new Promise<void>(resolve => { lost = resolve; });
    const prefix = this.name === 'hirsel' ? 'hirsel-' : `hirsel-${this.name}-`;
    const completed = navigator.locks.request(prefix + name, {mode: 'exclusive', ifAvailable: !wait}, async lock => {
      if (!lock) { grant(null); return; }
      grant({lost: ended, release: async () => { release(); await completed; }});
      await held;
    });
    void completed.then(lost, error => { fail(error); lost(); });
    return acquired;
  }
  private async database(): Promise<IDBDatabase> {
    if (this.db) return this.db;
    const request = indexedDB.open(this.name, 1);
    request.onupgradeneeded = () => request.result.createObjectStore('state');
    const db = await new Promise<IDBDatabase>((resolve, reject) => {
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
      request.onblocked = () => reject(new Error('IndexedDB blocked'));
    });
    db.onversionchange = () => { db.close(); this.db = undefined; };
    db.onclose = () => { this.db = undefined; };
    this.db = db;
    return db;
  }
  async read(): Promise<Installation | null> {
    const db = await this.database();
    return new Promise((resolve, reject) => {
      const tx = db.transaction('state', 'readonly'), request = tx.objectStore('state').get('installation');
      tx.oncomplete = () => {
        try {
          if (request.result === undefined) resolve(null);
          else if (typeof request.result !== 'string') reject(new CorruptState());
          else resolve(decodeState(request.result));
        } catch (error) { reject(error); }
      };
      tx.onabort = () => reject(tx.error ?? new Error('IndexedDB read aborted'));
    });
  }
  async write(state: Installation): Promise<void> {
    const encoded = encodeState(state), db = await this.database();
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction('state', 'readwrite', {durability: 'strict'});
      tx.objectStore('state').put(encoded, 'installation');
      tx.oncomplete = () => resolve();
      tx.onabort = () => reject(tx.error ?? new Error('IndexedDB commit aborted'));
    });
  }
  close(): void { this.db?.close(); this.db = undefined; }
}
