import assert from 'node:assert/strict';
import {openPlace, passkeyChallenge} from './generated/place.js';

const bytes = Uint8Array.from({length: 32}, (_, i) => 255 - i);
const encoded = Buffer.from(bytes).toString('base64url');
assert.deepEqual(passkeyChallenge(encoded), bytes);
assert.throws(() => passkeyChallenge(encoded + '='));
assert.throws(() => passkeyChallenge(encoded.slice(0, -1) + 'B'));
globalThis.location = new URL('https://client.test');
for (const url of ['https://other.test', 'http://client.test', 'https://user@client.test']) {
  await assert.rejects(openPlace(url), /InvalidPlaceOrigin/);
}
