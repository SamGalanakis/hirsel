// Babel's browser-compatible plugin still reads build-time environment flags.
// This module runs first and only inside the compiler worker.
if (!("process" in globalThis)) Object.assign(globalThis, { process: { env: { NODE_ENV: "production" } } });
