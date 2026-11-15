// Eatsights client helpers: JCS canonical JSON, Ed25519 signing, key ids, ULIDs and API calls.
// Works in browsers and in Node 22 (both expose WebCrypto as globalThis.crypto).

const subtle = globalThis.crypto.subtle;
const enc = new TextEncoder();

export function b64url(bytes) {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

export function fromB64url(str) {
  const s = atob(str.replace(/-/g, '+').replace(/_/g, '/'));
  return Uint8Array.from(s, (c) => c.charCodeAt(0));
}

// RFC 8785: object keys sorted by UTF-16 code units; ES number and string serialisation.
export function canonicalize(v) {
  if (v === null || typeof v !== 'object') {
    if (typeof v === 'number' && !Number.isFinite(v)) throw new Error('non-finite number');
    return JSON.stringify(v);
  }
  if (Array.isArray(v)) return '[' + v.map(canonicalize).join(',') + ']';
  const keys = Object.keys(v).filter((k) => v[k] !== undefined).sort();
  return '{' + keys.map((k) => JSON.stringify(k) + ':' + canonicalize(v[k])).join(',') + '}';
}

export function signingString(obj) {
  const { signature, ...rest } = obj;
  return canonicalize(rest);
}

export function ed25519Supported() {
  return subtle.generateKey({ name: 'Ed25519' }, true, ['sign', 'verify']).then(() => true, () => false);
}

export async function keyIdFromRaw(raw) {
  const digest = new Uint8Array(await subtle.digest('SHA-256', raw));
  return b64url(digest.slice(0, 16));
}

async function finish(privateKey, publicKey, seed) {
  const raw = new Uint8Array(await subtle.exportKey('raw', publicKey));
  return { privateKey, publicKey, seed, public_key: b64url(raw), key_id: await keyIdFromRaw(raw) };
}

export async function generateKey() {
  const kp = await subtle.generateKey({ name: 'Ed25519' }, true, ['sign', 'verify']);
  const pkcs8 = new Uint8Array(await subtle.exportKey('pkcs8', kp.privateKey));
  return finish(kp.privateKey, kp.publicKey, b64url(pkcs8.slice(-32)));
}

// Import a 32-byte Ed25519 seed (base64url, as printed by `eatsights keygen`).
export async function importSeed(seedB64) {
  const seed = fromB64url(seedB64);
  if (seed.length !== 32) throw new Error('seed must be 32 bytes');
  const prefix = [0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20];
  const pkcs8 = new Uint8Array([...prefix, ...seed]);
  const privateKey = await subtle.importKey('pkcs8', pkcs8, { name: 'Ed25519' }, true, ['sign']);
  const jwk = await subtle.exportKey('jwk', privateKey);
  const publicKey = await subtle.importKey('jwk', { kty: 'OKP', crv: 'Ed25519', x: jwk.x }, { name: 'Ed25519' }, true, ['verify']);
  return finish(privateKey, publicKey, seedB64);
}

export async function sign(key, obj) {
  const canonical = signingString(obj);
  const sig = new Uint8Array(await subtle.sign({ name: 'Ed25519' }, key.privateKey, enc.encode(canonical)));
  return { signed: { ...obj, signature: b64url(sig) }, canonical };
}

export async function verify(publicKeyB64, obj) {
  const pk = await subtle.importKey('raw', fromB64url(publicKeyB64), { name: 'Ed25519' }, false, ['verify']);
  return subtle.verify({ name: 'Ed25519' }, pk, fromB64url(obj.signature), enc.encode(signingString(obj)));
}

const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
export function ulid(prefix = '', now = Date.now()) {
  let t = '';
  for (let i = 0; i < 10; i++) { t = CROCKFORD[now % 32] + t; now = Math.floor(now / 32); }
  const r = globalThis.crypto.getRandomValues(new Uint8Array(16));
  let s = '';
  for (let i = 0; i < 16; i++) s += CROCKFORD[r[i] & 31];
  return prefix + t + s;
}

export const nowUtc = () => new Date().toISOString().replace(/\.\d{3}Z$/, 'Z');

// API client. Every call returns what was sent and what came back so the page can show both.
export function client(base = '') {
  async function call(method, path, body, headers = {}) {
    const init = { method, cache: 'no-store', headers: { accept: 'application/json', ...headers } };
    if (body !== undefined) { init.body = JSON.stringify(body); init.headers['content-type'] = 'application/json'; }
    const res = await fetch(base + path, init);
    const text = await res.text();
    let json = null;
    try { json = text ? JSON.parse(text) : null; } catch { json = text; }
    const h = {};
    for (const k of ['etag', 'cache-control', 'content-type', 'location']) if (res.headers.get(k)) h[k] = res.headers.get(k);
    return { request: { method, path, headers: init.headers, body }, status: res.status, headers: h, body: json };
  }
  return {
    get: (path, headers) => call('GET', path, undefined, headers),
    post: (path, body) => call('POST', path, body),
  };
}

export async function keyRegistration(key) {
  return sign(key, { type: 'key_registration', algorithm: 'ed25519', public_key: key.public_key, created_at: nowUtc() });
}

export async function entitySubmission(key, entity_type, entity, op = 'create') {
  return sign(key, {
    id: ulid('es_'), schema_version: '0.4', op, entity_type, entity,
    submitted_at: nowUtc(), contributor: { key_id: key.key_id }, license: 'CC0-1.0',
  });
}

export async function priceObservation(key, { place_id, menu_item_id, amount_minor, fulfilment = 'dine_in' }) {
  const t = nowUtc();
  return sign(key, {
    id: ulid('ob_'), schema_version: '0.3', kind: 'value', property: 'price.amount',
    subject: { scope: 'menu_item', place_id, menu_item_id },
    value: { amount_minor, currency: 'INR', fulfilment },
    observed_at: t, submitted_at: t, method: 'in_person', source_type: 'customer', prompt_type: 'unprompted',
    contributor: { key_id: key.key_id, checkin_verified: false }, license: 'CC0-1.0',
  });
}

// Minimal check of a value against the subset of JSON Schema used by price.amount.
export function checkValue(schema, value) {
  const errs = [];
  if (!schema || schema.type !== 'object') return errs;
  for (const r of schema.required || []) if (!(r in value)) errs.push(`missing ${r}`);
  for (const [k, v] of Object.entries(value)) {
    const p = schema.properties?.[k];
    if (!p) { if (schema.additionalProperties === false) errs.push(`unknown field ${k}`); continue; }
    if (p.type === 'integer' && !Number.isInteger(v)) errs.push(`${k} must be an integer`);
    if (p.minimum !== undefined && v < p.minimum) errs.push(`${k} must be >= ${p.minimum}`);
    if (p.enum && !p.enum.includes(v)) errs.push(`${k} must be one of ${p.enum.join(', ')}`);
    if (p.pattern && !new RegExp(p.pattern).test(v)) errs.push(`${k} must match ${p.pattern}`);
  }
  return errs;
}
