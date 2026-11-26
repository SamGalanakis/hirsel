// T5. Run inside the Boat VM with Playwright and both browsers installed.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { createInterface } from 'node:readline';
import { chromium, firefox } from 'playwright-core';

const executable = process.argv[2];
const originServer = createServer((_request, response) => {
  response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
  response.end('<!doctype html><title>WebTransport browser law</title>');
});
originServer.listen(0, '127.0.0.1');
await once(originServer, 'listening');
const origin = `http://127.0.0.1:${originServer.address().port}`;

function deadline(promise, milliseconds, operation) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${operation} timed out`)), milliseconds);
    }),
  ]).finally(() => clearTimeout(timer));
}

async function runBrowser(name, kind) {
  const child = spawn(executable, [origin], { stdio: ['ignore', 'pipe', 'inherit'] });
  const exited = once(child, 'exit');
  let onLine;
  let closeLine;
  const readiness = new Promise((resolve) => { onLine = resolve; });
  const closing = new Promise((resolve) => { closeLine = resolve; });
  const lines = createInterface({ input: child.stdout });
  lines.on('line', (line) => {
    console.log(`${name} server: ${line}`);
    if (line.startsWith('ready ')) onLine(line);
    if (line.startsWith('closed ')) closeLine(line);
  });

  let browser;
  try {
    const readyLine = await deadline(Promise.race([
      readiness,
      exited.then(([code]) => { throw new Error(`echo server exited before ready (${code})`); }),
    ]), 10000, 'echo server startup');
    const [, port, ...hashes] = readyLine.split(' ');
    assert.equal(hashes.length, 2);
    hashes.forEach((hash) => assert.match(hash, /^[a-f0-9]{64}$/));
    browser = await kind.launch();
    console.log(`${name} ${browser.version()}`);
    const page = await browser.newPage();
    page.on('console', (event) => console.log(`${name} page: ${event.text()}`));
    await page.goto(origin);
    const result = await deadline(page.evaluate(async ({ port, hashes }) => {
      if (!globalThis.WebTransport) throw new Error('WebTransport is unavailable');
      const serverCertificateHashes = hashes.map((hex) => ({
        algorithm: 'sha-256',
        value: Uint8Array.from(hex.match(/../g), (octet) => parseInt(octet, 16)),
      }));
      const wt = new WebTransport(`https://127.0.0.1:${port}/echo`, {
        serverCertificateHashes,
        requireUnreliable: true,
      });
      void wt.closed.catch(() => {});
      await wt.ready;
      console.log('WebTransport ready');

      const input = new Uint8Array(10 * 1024 * 1024);
      for (let i = 0; i < input.length; i += 1) input[i] = i ^ (i >>> 8) ^ (i >>> 16);
      const stream = await wt.createBidirectionalStream();
      const writer = stream.writable.getWriter();
      const reader = stream.readable.getReader();
      const output = new Uint8Array(input.length);
      const [, size] = await Promise.all([
        (async () => {
          for (let i = 0; i < input.length; i += 65536) {
            await writer.write(input.subarray(i, i + 65536));
          }
          await writer.close();
        })(),
        (async () => {
          let offset = 0;
          for (;;) {
            const { done, value } = await reader.read();
            if (done) return offset;
            output.set(value, offset);
            offset += value.length;
          }
        })(),
      ]);
      const hash = async (bytes) => Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)));
      const inputHash = await hash(input);
      const outputHash = await hash(output);
      console.log(`stream echoed: ${size} bytes`);

      const tx = wt.datagrams.writable.getWriter();
      const rx = wt.datagrams.readable.getReader();
      const sent = new Uint8Array(1000 * 32);
      const received = new Uint8Array(sent.length);
      for (let i = 0; i < 1000; i += 1) {
        const first = i * 32;
        const datagram = sent.subarray(first, first + 32);
        for (let j = 0; j < datagram.length; j += 1) datagram[j] = i ^ (i >>> 8) ^ (31 * j);
        datagram[0] = i >>> 8;
        datagram[1] = i & 255;
        await tx.write(datagram);
        const { done, value } = await rx.read();
        if (done || value.length !== 32) throw new Error(`datagram ${i} did not echo 32 bytes`);
        received.set(value, first);
      }
      tx.releaseLock();
      rx.releaseLock();
      const sentHash = await hash(sent);
      const receivedHash = await hash(received);
      wt.close({ closeCode: 7, reason: 'bye' });
      const { closeCode, reason } = await wt.closed;
      return { size, inputHash, outputHash, sentHash, receivedHash, closeCode, reason };
    }, { port, hashes }), 180000, `${name} browser_echo`);

    assert.equal(result.size, 10 * 1024 * 1024);
    assert.deepEqual(result.outputHash, result.inputHash);
    assert.deepEqual(result.receivedHash, result.sentHash);
    assert.equal(result.closeCode, 7);
    assert.equal(result.reason, 'bye');
    assert.equal(await deadline(closing, 10000, 'server close event'), 'closed 7 627965');
    const [code] = await deadline(exited, 10000, 'echo server exit');
    assert.equal(code, 0);
    console.log(`PASS ${name} browser_echo, 10485760 stream bytes, 1000 datagrams, close 7 bye`);
  } finally {
    await browser?.close();
    if (child.exitCode === null && child.signalCode === null) {
      child.kill();
      await exited;
    }
    lines.close();
  }
}

try {
  const rows = await Promise.allSettled([runBrowser('chromium', chromium), runBrowser('firefox', firefox)]);
  for (const [i, row] of rows.entries()) {
    if (row.status === 'rejected') {
      console.error(`FAIL ${['chromium', 'firefox'][i]} browser_echo: ${row.reason.stack ?? row.reason}`);
      process.exitCode = 1;
    }
  }
} finally {
  originServer.close();
}
