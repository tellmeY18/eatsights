// Checks the demo's crypto against the eatsights CLI. Run: node --test demo/test/
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import * as es from '../eatsights.js';

const BIN = process.env.EATSIGHTS_BIN || fileURLToPath(new URL('../../target/release/eatsights', import.meta.url));
const cli = (args, input) => JSON.parse(execFileSync(BIN, args, { input }).toString());

test('JCS canonical form', () => {
  assert.equal(es.canonicalize({ b: 1, a: [true, null, 1.5e-7, 'é\u2028"'], A: { z: 1e21, y: -0 } }),
    '{"A":{"y":0,"z":1e+21},"a":[true,null,1.5e-7,"é\u2028\\""],"b":1}');
  assert.equal(es.canonicalize({ '\u20ac': 1, '\ud83d\ude00': 2, '\ufb33': 3 }), '{"\u20ac":1,"\ud83d\ude00":2,"\ufb33":3}');
});

test('key_id, public key and signatures match the CLI byte for byte', async () => {
  const k = cli(['keygen']);
  const mine = await es.importSeed(k.private_key);
  assert.equal(mine.public_key, k.public_key);
  assert.equal(mine.key_id, k.key_id);
  assert.equal(mine.key_id.length, 22);
  assert.equal(cli(['pubkey', `--key=${k.private_key}`]).key_id, k.key_id);

  const obj = { z: 'ünïcode ☕', '\ufb33': 1, '\ud83d\ude00': 2, '\u20ac': 3, n: [1e21, 1.5e-7, -0, 0.1], a: { y: [3, 2.5, { b: null, a: false }], x: 1700 }, id: 'ob_01M7BHQY50D2K939M5G5K824XX' };
  const fromCli = cli(['sign', `--key=${k.private_key}`], JSON.stringify(obj));
  const { signed } = await es.sign(mine, obj);
  assert.equal(signed.signature.length, 86);
  assert.equal(signed.signature, fromCli.signature);
  assert.ok(await es.verify(k.public_key, fromCli));
  assert.ok(!(await es.verify(k.public_key, { ...fromCli, z: 'tampered' })));
});

test('generated browser keys round-trip through their seed', async () => {
  const k = await es.generateKey();
  const again = await es.importSeed(k.seed);
  assert.equal(again.key_id, k.key_id);
  assert.equal(cli(['pubkey', `--key=${k.seed}`]).key_id, k.key_id);
});

test('ULIDs', () => {
  const id = es.ulid('pl_');
  assert.match(id, /^pl_[0-9A-HJKMNP-TV-Z]{26}$/);
  assert.ok(es.ulid('', 1) < es.ulid('', 2));
});
