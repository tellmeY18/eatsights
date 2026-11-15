// Live end-to-end run against a server using only demo/eatsights.js.
// Usage: node demo/test/live.mjs [base]   (default https://develop.letsimpl.fyi)
// Writes public, permanent CC0 data.
import * as es from '../eatsights.js';

const base = process.argv[2] || 'https://develop.letsimpl.fyi';
const api = es.client(base);
const log = (label, r) => console.log(label.padEnd(34), r.status, JSON.stringify(r.body).slice(0, 220));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const fail = (m) => { console.error('FAIL', m); process.exit(1); };

const keys = await Promise.all(Array.from({ length: Number(process.env.KEYS || 3) }, () => es.generateKey()));
for (const k of keys) {
  const r = await api.post('/v1/keys', (await es.keyRegistration(k)).signed);
  log(`register ${k.key_id}`, r);
  if (r.body?.key_id !== k.key_id) fail('register');
}
const [A] = keys;
const pl = es.ulid('pl_'), sc = es.ulid('sc_'), mi = es.ulid('mi_');
const batch = [
  await es.entitySubmission(A, 'place', { id: pl, names: { en: 'Corner tea stall (demo)' }, category: 'tea_stall', location: { lat: 9.9816, lng: 76.2999 }, city: 'ernakulam' }),
  await es.entitySubmission(A, 'section', { id: sc, place_id: pl, labels: { en: 'Drinks' }, kind: 'drinks', sort: 1 }),
  await es.entitySubmission(A, 'menu_item', { id: mi, place_id: pl, section_id: sc, labels: { en: 'Tea' } }),
].map((x) => x.signed);
let r = await api.post('/v1/entities', batch); log('create place/section/item', r);
if (!r.body.results.every((x) => x.status === 'accepted')) fail('entities');

async function waitFact(pred, label) {
  for (let i = 0; i < 40; i++) {
    const f = await api.get(`/v1/facts?menu_item_id=${mi}&property=price.amount`);
    const fact = f.body.items?.[0];
    if (fact && pred(fact)) { console.log(label.padEnd(34), fact.status, 'confirmations', fact.counts.confirmations, 'confidence', fact.confidence); return fact; }
    await sleep(500);
  }
  fail(`timeout: ${label}`);
}

const obs = [];
for (const [i, k] of keys.entries()) {
  const o = (await es.priceObservation(k, { place_id: pl, menu_item_id: mi, amount_minor: 1500 })).signed;
  obs.push(o);
  r = await api.post('/v1/submissions', [o]); log(`price by key ${i + 1}`, r);
  if (r.body.results[0].status !== 'accepted') fail('submission');
  await waitFact((f) => f.counts?.confirmations === i + 1, `fact after ${i + 1} key(s)`);
}
r = await api.post('/v1/submissions', [obs[0]]); log('resubmit', r);
const tampered = { ...obs[0], value: { ...obs[0].value, amount_minor: 10 } };
r = await api.post('/v1/submissions', [tampered]); log('tampered', r);
r = await api.get(`/v1/observations?menu_item_id=${mi}`);
const text = JSON.stringify(r.body);
console.log('public observations', r.body.items.length, 'leak key_id?', keys.some((k) => text.includes(k.key_id)), 'leak signature?', text.includes('signature'));
r = await api.get(`/v1/places/${pl}/menu`); log('menu', r);
console.log('place', pl);
