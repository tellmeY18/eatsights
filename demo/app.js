// Eatsights live demo. Temporary, develop only; not part of the product (see README.md).
import { h, render } from '/demo/vendor/preact.module.js';
import htm from '/demo/vendor/htm.module.js';
import * as es from '/demo/eatsights.js';
import { wikidata, mapTiles } from '/demo/enrich.js';

const html = htm.bind(h);
const BASE = new URLSearchParams(location.search).get('api') || '';
const api = es.client(BASE);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const pretty = (v) => JSON.stringify(v, null, 2);

// ---- state: one object, re-render on every change
const S = {
  ready: null, seq: null, edSupported: null,
  city: 'ernakulam', q: '', cell: '', places: null, placesCall: null,
  place: null, menu: null, menuCall: null, revalidate: null,
  registry: null,
  keys: [], keyCall: null,
  form: { name: 'My test stall', ml: '', category: 'tea_stall', city: 'ernakulam', lat: '9.9816', lng: '76.2999', section: 'Drinks', item: 'Tea', dish: 'Tea', wikidata: 'Q6097', wdStatus: 'verified', price: '15' },
  createdOsm: null, osmLinkCall: null,
  created: null, createCall: null,
  priceCall: null, priceSent: null, fact: null, factHistory: [], busy: false,
  dupCall: null, tamperCall: null,
  privacy: null,
  feed: [], feedAfter: null, exportCall: null,
  dishes: null, dish: null, dishFacts: null, merged: null,
  dishById: {}, wiki: {}, wikiOpen: {},
  error: null,
};
const set = (patch) => { Object.assign(S, patch); draw(); };
const run = (fn) => async (...a) => {
  if (S.busy) return;
  set({ busy: true, error: null });
  try { await fn(...a); } catch (e) { set({ error: String(e?.message || e) }); } finally { set({ busy: false }); }
};

// ---- helpers
const seqFromEtag = (etag) => { const m = /"(\d+)"/.exec(etag || ''); return m ? Number(m[1]) : null; };
const rupees = (minor) => 'Rs ' + (minor / 100).toLocaleString('en-IN', { maximumFractionDigits: 2 });
const label = (m) => (m && (m.en || Object.values(m)[0])) || '—';

const B32 = '0123456789bcdefghjkmnpqrstuvwxyz';
function geohash(lat, lng, precision = 6) {
  let lo = [-90, -180], hi = [90, 180], even = true, bit = 0, ch = 0, out = '';
  while (out.length < precision) {
    const i = even ? 1 : 0, v = even ? lng : lat, mid = (lo[i] + hi[i]) / 2;
    if (v >= mid) { ch = (ch << 1) | 1; lo[i] = mid; } else { ch <<= 1; hi[i] = mid; }
    even = !even;
    if (++bit === 5) { out += B32[ch]; bit = 0; ch = 0; }
  }
  return out;
}

function fmtValue(prop, v) {
  if (v == null) return '—';
  if ('amount_minor' in v) return rupees(v.amount_minor) + (v.fulfilment && v.fulfilment !== 'dine_in' ? ` ${v.fulfilment}` : '') + (v.valid_during ? ` (${v.valid_during})` : '') + (v.tax_included ? ' incl. tax' : '');
  if ('seconds' in v) return `${Math.round(v.seconds / 60)} min ${v.stage.replace(/_/g, ' ')}`;
  if ('local_time' in v) return `${v.local_time} on ${v.day_type}s`;
  if ('score' in v) return `${v.score}/5`;
  if ('level' in v) return `sweetness ${v.level}/4`;
  if ('value' in v && 'unit' in v) return `${v.value} ${v.unit}`;
  const list = v.methods || v.kinds || v.options || v.items || v.allergens;
  if (list) return list.join(', ').replace(/_/g, ' ');
  return String(v.state || v.type || v.bucket || v.diet || v.rule || JSON.stringify(v)).replace(/_/g, ' ');
}

const FactLine = ({ f }) => html`<div class="facts"><code>${f.property}</code> · ${fmtValue(f.property, f.current_value)} ${factChip(f)}${f.vendor_value_differs ? html` <span class="chip contested">⚠ Vendor says otherwise</span>` : null}</div>`;

function factChip(f) {
  if (!f) return html`<span class="chip unconfirmed">○ No reports</span>`;
  const n = f.counts?.distinct_contributors ?? 0;
  const pct = Math.round((f.confidence || 0) * 100);
  const map = {
    confirmed: ['✓', `Confirmed · ${n} people`],
    unconfirmed: ['○', `Unconfirmed · ${n} ${n === 1 ? 'person' : 'people'}`],
    contested: ['⚠', 'Reports disagree'],
    stale: ['◌', 'Not confirmed recently'],
  };
  const [icon, words] = map[f.status] || ['?', f.status];
  return html`<span class=${'chip ' + f.status + (f.status === 'confirmed' ? ' crayon' : '')}><span aria-hidden="true">${icon}</span>${words} · ${pct}%</span>`;
}

function Slip({ title, call, open }) {
  if (!call) return null;
  const req = call.request;
  const sent = req.body !== undefined ? pretty(req.body) : '';
  return html`<details class="slip" open=${open}>
    <summary>${title} <span class="chip">${call.status}</span></summary>
    <div class="paper">
      <pre>${req.method} ${BASE}${req.path}${sent ? '\n\n' + sent : ''}</pre>
    </div>
    <div class="paper">
      <pre>HTTP ${call.status}\n${Object.entries(call.headers).map(([k, v]) => `${k}: ${v}`).join('\n')}\n\n${typeof call.body === 'string' ? call.body : pretty(call.body)}</pre>
    </div>
  </details>`;
}

const Problem = ({ call }) => {
  const items = call?.body?.results;
  if (!items) return null;
  return html`<ul class="list" aria-live="polite">${items.map((r) => html`<li><p>
    <span class=${'chip' + (r.status === 'rejected' ? ' error' : r.status === 'accepted' ? ' crayon' : '')}>${r.status === 'accepted' ? '✓' : r.status === 'duplicate' ? '=' : '✕'} ${r.status}</span>
    <code> ${r.id}</code>${r.problem ? html`<br/><small>${r.problem.status} ${r.problem.title}: ${r.problem.detail}</small>` : null}</p></li>`)}</ul>`;
};

// ---- actions
async function status() {
  const r = await api.get('/readyz');
  const reg = await api.get('/v1/registry/properties');
  set({ ready: r.status === 200, seq: seqFromEtag(reg.headers.etag), registry: reg });
  es.ed25519Supported().then((ok) => set({ edSupported: ok }));
}

const search = run(async (mode) => {
  let path = '/v1/places?limit=50';
  if (mode === 'cell' && S.cell) path += '&cell=' + encodeURIComponent(S.cell);
  else {
    if (S.city) path += '&city=' + encodeURIComponent(S.city);
    if (S.q) path += '&q=' + encodeURIComponent(S.q);
  }
  const r = await api.get(path);
  set({ places: r.body?.items || [], placesCall: r });
});

const openPlace = run(async (p) => {
  set({ place: p, menu: null, revalidate: null, cell: geohash(p.location.lat, p.location.lng, 6) });
  const r = await api.get(`/v1/places/${p.id}/menu`);
  set({ menu: r.status === 200 ? r.body : null, menuCall: r });
  const ids = [...new Set((r.body?.items || []).map((it) => it.item.dish_id).filter((id) => id && !S.dishById[id]))];
  const got = await Promise.all(ids.map((id) => api.get(`/v1/dishes/${id}`)));
  const add = {};
  for (const d of got) if (d.status === 200) add[d.body.id] = d.body;
  set({ dishById: { ...S.dishById, ...add } });
});

const revalidate = run(async () => {
  const etag = S.menuCall?.headers?.etag;
  const r = await api.get(`/v1/places/${S.place.id}/menu`, etag ? { 'if-none-match': etag } : {});
  set({ revalidate: r });
});

async function newKey() {
  const key = await es.generateKey();
  const { signed, canonical } = await es.keyRegistration(key);
  const r = await api.post('/v1/keys', signed);
  if (r.status >= 300) throw new Error(`key registration failed: ${r.status} ${r.body?.detail || ''}`);
  return { key, call: r, canonical };
}

const makeKey = run(async () => {
  const { key, call, canonical } = await newKey();
  set({ keys: [key], keyCall: { ...call, canonical } });
});

const createPlace = run(async () => {
  const k = S.keys[0];
  if (!k) throw new Error('make a key first (step 4)');
  const f = S.form;
  const lat = Number(f.lat), lng = Number(f.lng);
  if (!f.name.trim() || !Number.isFinite(lat) || !Number.isFinite(lng)) throw new Error('name, lat and lng are required');
  const place = {
    id: es.ulid('pl_'), names: { en: 'Demo · ' + f.name.trim().replace(/^Demo · /, ''), ...(f.ml.trim() ? { ml: f.ml.trim() } : {}) },
    category: f.category, location: { lat, lng, precision_m: 25 }, city: f.city.trim() || 'ernakulam',
  };
  const section = { id: es.ulid('sc_'), place_id: place.id, labels: { en: f.section.trim() || 'Menu' }, kind: 'other', sort: 1 };
  const qid = f.wikidata.trim().toUpperCase();
  if (qid && !/^Q[1-9]\d*$/.test(qid)) throw new Error('a Wikidata id looks like Q6097');
  const dish = f.dish.trim() ? {
    id: es.ulid('di_'), labels: { en: 'Demo · ' + f.dish.trim().replace(/^Demo · /, '') }, kind: 'dish',
    ...(qid ? { wikidata: { id: qid, status: f.wdStatus, ...(f.wdStatus === 'verified' ? { verified_at: new Date().toISOString().slice(0, 10) } : {}) } } : {}),
  } : null;
  const item = { id: es.ulid('mi_'), place_id: place.id, section_id: section.id, labels: { en: f.item.trim() || 'Item' }, ...(dish ? { dish_id: dish.id } : {}) };
  const subs = [];
  const list = [['place', place], ['section', section], ...(dish ? [['dish', dish]] : []), ['menu_item', item]];
  for (const [t, e] of list) subs.push((await es.entitySubmission(k, t, e)).signed);
  const r = await api.post('/v1/entities', subs);
  set({ createCall: r, created: { place, section, item, dish }, createdOsm: null, osmLinkCall: null, fact: null, factHistory: [], priceCall: null, dupCall: null, tamperCall: null, privacy: null });
  watchOsm(place.id);
});

// The OSM checker (a worker) searches around every new pin. Watch it change the place's osm state.
async function watchOsm(id) {
  for (let i = 0; i < 60 && S.created?.place.id === id; i++) {
    const r = await api.get(`/v1/places/${id}`);
    set({ createdOsm: r.body?.osm || null });
    if (r.body?.osm && r.body.osm.state !== 'unchecked') return;
    await sleep(2000);
  }
}

// Confirming an OSM link is an admin action: only root or verifier keys may sign it. Sending it
// with a browser key shows the request format and the permission check (403).
const tryLinkOsm = run(async () => {
  const k = S.keys[0];
  const cand = S.createdOsm?.candidates?.[0];
  if (!k || !cand) throw new Error('needs a key and an OSM candidate');
  const { signed } = await es.sign(k, { id: es.ulid('es_'), action: 'link_osm', params: { place_id: S.created.place.id, osm_id: cand.id }, issued_at: es.nowUtc(), issuer_key_id: k.key_id });
  set({ osmLinkCall: await api.post('/v1/admin/osm-links', signed) });
});

async function submitPrice(key) {
  const { place, item } = S.created;
  const amount_minor = Math.round(Number(S.form.price) * 100);
  const schema = S.registry?.body?.items?.find((p) => p.id === 'price.amount')?.value_schema;
  const value = { amount_minor, currency: 'INR', fulfilment: 'dine_in' };
  const errs = es.checkValue(schema, value);
  if (errs.length) throw new Error('value does not match price.amount: ' + errs.join('; '));
  const { signed } = await es.priceObservation(key, { place_id: place.id, menu_item_id: item.id, amount_minor });
  const r = await api.post('/v1/submissions', [signed]);
  return { r, signed };
}

async function waitFact(prevCount) {
  const { item } = S.created;
  for (let i = 0; i < 40; i++) {
    const r = await api.get(`/v1/facts?menu_item_id=${item.id}&property=price.amount`);
    const f = r.body?.items?.[0];
    if (f && (f.counts?.distinct_contributors ?? 0) > prevCount) return f;
    await sleep(500);
  }
  throw new Error('the worker did not produce a fact in 20 s');
}

const reportPrice = run(async () => {
  if (!S.created) throw new Error('add a place first (step 5)');
  const { r, signed } = await submitPrice(S.keys[0]);
  set({ priceCall: r, priceSent: signed });
  const f = await waitFact(0);
  set({ fact: f, factHistory: [{ people: f.counts.distinct_contributors, status: f.status, confidence: f.confidence }] });
});

const anotherPerson = run(async () => {
  const { key } = await newKey();
  set({ keys: [...S.keys, key] });
  const prev = S.fact?.counts?.distinct_contributors ?? 0;
  await submitPrice(key);
  const f = await waitFact(prev);
  set({ fact: f, factHistory: [...S.factHistory, { people: f.counts.distinct_contributors, status: f.status, confidence: f.confidence }] });
});

const resubmit = run(async () => {
  const r = await api.post('/v1/submissions', [S.priceSent]);
  set({ dupCall: r });
});

const tamper = run(async () => {
  const t = structuredClone(S.priceSent);
  t.value.amount_minor += 100;
  const r = await api.post('/v1/submissions', [t]);
  set({ tamperCall: r });
});

const checkPrivacy = run(async () => {
  const { item } = S.created;
  const r = await api.get(`/v1/observations?menu_item_id=${item.id}&limit=20`);
  const pub = r.body?.items?.find((o) => o.id === S.priceSent.id);
  const text = JSON.stringify(r.body);
  const leaks = S.keys.filter((k) => text.includes(k.key_id)).length;
  set({ privacy: { call: r, pub, leaks, sigLeak: text.includes('"signature"') } });
});

const loadExport = run(async () => set({ exportCall: await api.get('/v1/export') }));

const loadDishes = run(async () => {
  const r = await api.get('/v1/dishes?limit=100');
  set({ dishes: r });
});

const openDish = run(async (d) => {
  const r = await api.get(`/v1/facts?dish_id=${d.id}&limit=50`);
  set({ dish: d, dishFacts: r.body?.items || [] });
  if (d.wikidata && !S.wiki[d.wikidata.id]) {
    set({ wiki: { ...S.wiki, [d.wikidata.id]: { loading: true } } });
    wikidata(d.wikidata.id).then(
      (data) => set({ wiki: { ...S.wiki, [d.wikidata.id]: { data } } }),
      (e) => set({ wiki: { ...S.wiki, [d.wikidata.id]: { error: String(e.message || e) } } }),
    );
  }
});

// Merges are in the change feed (op "merge"); `follow=false` returns the merged record itself.
const findMerged = run(async () => {
  const seen = new Set();
  let after = 0;
  for (let i = 0; i < 20; i++) {
    const r = await api.get(`/v1/changes?after=${after}&limit=500`);
    for (const c of r.body?.items || []) if (c.entity_type === 'place' && c.op === 'merge') seen.add(c.entity_id);
    if (!r.body?.items?.length || r.body.next_after === after) break;
    after = r.body.next_after;
  }
  const out = [];
  for (const id of seen) {
    const raw = await api.get(`/v1/places/${id}?follow=false`);
    if (raw.body?.record_state !== 'merged') continue;
    const followed = await fetch(`${BASE}/v1/places/${id}`, { cache: 'no-store' });
    out.push({ place: raw.body, raw, redirected: followed.redirected, finalUrl: followed.url });
  }
  set({ merged: out });
});

const WD_STATUS = { verified: ['✓', 'Verified link'], unverified: ['○', 'Unverified link'], broken: ['✕', 'Broken link'] };
function Wikidata({ link }) {
  if (!link) return html`<span class="chip unconfirmed">○ No Wikidata item</span>`;
  const [icon, words] = WD_STATUS[link.status] || ['?', link.status];
  return html`<span class=${'chip ' + (link.status === 'verified' ? 'confirmed crayon' : link.status === 'broken' ? 'error' : 'unconfirmed')}>${icon} ${words}</span>
    ${link.status === 'broken' ? html` <code>${link.id}</code>` : html` <a class="outbound" href=${'https://www.wikidata.org/wiki/' + link.id} rel="noopener noreferrer" target="_blank">${link.id} on Wikidata ↗</a>`}`;
}

const OSM_STATE = {
  unchecked: ['…', 'Not searched yet'],
  unmapped: ['○', 'Not on OpenStreetMap'],
  candidate: ['?', 'Possible OSM matches'],
  linked: ['✓', 'Linked to OSM'],
};
// Wikidata / Wikipedia / Commons, read in the browser on demand (never stored in Eatsights).
function toggleWiki(key, qid) {
  const open = !S.wikiOpen[key];
  set({ wikiOpen: { ...S.wikiOpen, [key]: open } });
  if (!open || S.wiki[qid]) return;
  set({ wiki: { ...S.wiki, [qid]: { loading: true } } });
  wikidata(qid).then(
    (data) => set({ wiki: { ...S.wiki, [qid]: { data } } }),
    (e) => set({ wiki: { ...S.wiki, [qid]: { error: String(e.message || e) } } }),
  );
}

function WikiPanel({ qid }) {
  const w = S.wiki[qid];
  if (!w || w.loading) return html`<p aria-live="polite">Reading ${qid} from Wikidata…</p>`;
  if (w.error) return html`<p class="chip error" role="status">✕ ${w.error}. A broken link stays stored until someone fixes it.</p>`;
  const d = w.data;
  return html`<div class="wiki">
    ${d.image ? html`<figure><img src=${d.image.src} alt=${d.label || d.qid} loading="lazy" referrerpolicy="strict-origin-when-cross-origin"/>
      <figcaption><a href=${d.image.page} rel="noopener noreferrer" target="_blank">${d.image.artist} · ${d.image.licence} · Wikimedia Commons ↗</a></figcaption></figure>` : html`<p class="muted">No Commons image on this item.</p>`}
    <div>
      <p><strong>${d.label || d.qid}</strong>${d.labelMl ? html` · <span lang="ml">${d.labelMl}</span>` : null}</p>
      ${d.description ? html`<p class="muted">${d.description}</p>` : null}
      ${d.wikipedia ? html`<p lang=${d.wikipedia.lang}>${d.wikipedia.extract}</p>
        <p><a href=${d.wikipedia.url} rel="noopener noreferrer" target="_blank">${d.wikipedia.title} on ${d.wikipedia.lang}.wikipedia.org ↗</a></p>` : html`<p class="muted">No Wikipedia article linked.</p>`}
      <p><a href=${d.wikidataUrl} rel="noopener noreferrer" target="_blank">${d.qid} on Wikidata ↗</a></p>
      <small>Read live from Wikidata, Wikipedia and Commons. Eatsights stores only the id ${d.qid}.</small>
    </div>
  </div>`;
}

function DishLine({ dish, k }) {
  if (!dish) return null;
  const link = dish.wikidata;
  return html`<div class="facts">dish <strong>${label(dish.labels)}</strong> <${Wikidata} link=${link}/>
    ${link ? html` <button class="quiet" aria-expanded=${!!S.wikiOpen[k]} onClick=${() => toggleWiki(k, link.id)}>${S.wikiOpen[k] ? 'Hide' : 'Show'} Wikipedia + image</button>` : null}
    ${link && S.wikiOpen[k] ? html`<${WikiPanel} qid=${link.id}/>` : null}</div>`;
}

// Static map from the place's own pin, drawn with OpenStreetMap tiles in the browser.
function PinMap({ lat, lng, zoom = 17 }) {
  const W = 480, H = 280;
  const tiles = mapTiles(lat, lng, zoom, W, H);
  return html`<figure class="map-wrap">
    <div class="map" role="img" aria-label=${`Map around the pin ${lat.toFixed(4)}, ${lng.toFixed(4)}`}>
      <div class="map-inner">
        ${tiles.map((t) => html`<img key=${t.key} src=${t.src} alt="" loading="lazy" referrerpolicy="strict-origin-when-cross-origin" style=${{ left: t.left + 'px', top: t.top + 'px' }}/>`)}
        <span class="map-pin" aria-hidden="true">▼</span>
      </div>
    </div>
    <figcaption>Pin ${lat.toFixed(5)}, ${lng.toFixed(5)} (Eatsights' own data) ·
      <a href=${`https://www.openstreetmap.org/?mlat=${lat}&mlon=${lng}#map=18/${lat}/${lng}`} rel="noopener noreferrer" target="_blank">open map ↗</a> ·
      tiles © <a href="https://www.openstreetmap.org/copyright" rel="noopener noreferrer" target="_blank">OpenStreetMap contributors</a></figcaption>
  </figure>`;
}

function OsmBlock({ osm }) {
  if (!osm) return null;
  const [icon, words] = OSM_STATE[osm.state] || ['?', osm.state];
  return html`<div>
    <h3>On the map</h3>
    <div class="facts"><span class=${'chip ' + (osm.state === 'linked' ? 'confirmed crayon' : 'unconfirmed')}>${icon} ${words}</span>
      ${osm.ready_to_map ? html` <span class="chip crayon">⚑ Ready to map</span>` : null}</div>
    ${osm.id ? html`<div class="facts">OSM <code>${osm.id}</code> · confirmed by a person on ${osm.verified_at}</div>` : null}
    ${osm.checked_at ? html`<div class="facts">searched ${osm.checked_at}</div>` : null}
    ${osm.candidates?.length ? html`<div class="facts">${osm.candidates.map((c) => html`<div><code>${c.id}</code> · ${Math.round(c.distance_m)} m away · name match ${Math.round((c.name_score || 0) * 100)}%</div>`)}</div>` : null}
    <small>Only OSM ids, distances and match scores are stored, never OSM names, tags or coordinates. On develop the OSM search is a dummy service, so these ids are fake.</small>
  </div>`;
}

async function pollFeed() {
  for (;;) {
    try {
      if (S.feedAfter === null) {
        const seq = S.seq ?? 0;
        set({ feedAfter: Math.max(0, seq - 15) });
      }
      const r = await api.get(`/v1/changes?after=${S.feedAfter}&limit=100`);
      const items = r.body?.items || [];
      if (items.length) set({ feed: [...items.reverse(), ...S.feed].slice(0, 60), feedAfter: r.body.next_after ?? S.feedAfter, seq: r.body.next_after ?? S.seq });
    } catch { /* keep polling */ }
    await sleep(4000);
  }
}

// ---- view
function Field({ name, label: l, type = 'text', ...rest }) {
  return html`<label>${l}<input type=${type} value=${S.form[name]} onInput=${(e) => { S.form[name] = e.currentTarget.value; }} ...${rest}/></label>`;
}

function App() {
  const busy = S.busy;
  return html`<main>
    <header>
      <p class="muted">develop · temporary demo</p>
      <h1>Eatsights</h1>
      <p>Open food data as an API: menus, prices, availability, wait times. Anyone reads, anyone writes with a signed key, and everything is CC0.</p>
      <nav class="links" aria-label="API documentation">
        <a class="outbound" href=${BASE + '/openapi.json'}>OpenAPI ↗</a>
        <a class="outbound" href=${BASE + '/docs/'}>Swagger UI ↗</a>
        <a class="outbound" href=${BASE + '/schema/v0.3/schema.json'}>JSON Schemas ↗</a>
        <a class="outbound" href="https://github.com/tellmeY18/eatsights">Source ↗</a>
      </nav>
      <div class="status" aria-live="polite">
        <span class="chip">${S.ready === null ? '… checking' : S.ready ? '● API ready' : '✕ API down'}</span>
        <span class="chip">Δ change ${S.seq ?? '…'}</span>
        <span class="chip">${S.edSupported === false ? '✕ No Ed25519 in this browser' : '🔑 Browser signing'}</span>
      </div>
      ${S.error ? html`<p class="chip error" role="alert">✕ ${S.error}</p>` : null}
    </header>

    <section aria-labelledby="s1">
      <h2 id="s1"><span class="n">1</span>Browse places and menus</h2>
      <p class="lead">Public reads need no key. Every response is cacheable: ETags, 304s and pre-built menu documents.</p>
      <p class="chip">ⓘ Demo data: every place named “Demo · …” is fictional. Try Kaloor Chaya Kada for every kind of fact.</p>
      <div class="row">
        ${['ernakulam', 'thrissur', 'kozhikode'].map((c) => html`<button class="quiet" disabled=${busy} onClick=${() => { S.city = c; S.q = ''; search('city'); }}>${c}</button>`)}
      </div>
      <div class="row">
        <label>City<input value=${S.city} onInput=${(e) => { S.city = e.currentTarget.value; }}/></label>
        <label>Name contains<input value=${S.q} onInput=${(e) => { S.q = e.currentTarget.value; }}/></label>
        <button class="primary" disabled=${busy} onClick=${() => search('city')}>Search</button>
      </div>
      <div class="row">
        <label>Geohash cell<input value=${S.cell} placeholder="t9y" onInput=${(e) => { S.cell = e.currentTarget.value; }}/></label>
        <button disabled=${busy || !S.cell} onClick=${() => search('cell')}>Search cell</button>
      </div>
      ${S.places ? html`<p class="muted" aria-live="polite">${S.places.length} places</p>
        <ul class="list">${S.places.map((p) => html`<li><button class="pick" aria-pressed=${S.place?.id === p.id} onClick=${() => openPlace(p)}>
          <span>${label(p.names)}${p.names.ml ? html` · <span lang="ml">${p.names.ml}</span>` : null}</span><small>${p.category} · ${p.city}</small></button></li>`)}</ul>` : null}
      ${S.menu ? html`<div class="paper" aria-label="Menu">
        <h3>${label(S.menu.place.names)}</h3>
        ${S.menu.place.names.ml ? html`<p lang="ml">${S.menu.place.names.ml}</p>` : null}
        <small>${S.menu.place.category} · ${S.menu.place.city} · built ${S.menu.built_at} · ${S.menu.place.id}</small>
        <${PinMap} lat=${S.menu.place.location.lat} lng=${S.menu.place.location.lng}/>
        <${OsmBlock} osm=${S.menu.place.osm}/>
        ${S.menu.place_facts?.length ? html`<h3>About the place</h3>${S.menu.place_facts.map((f) => html`<${FactLine} f=${f}/>`)}` : null}
        ${(S.menu.sections.length ? S.menu.sections : [{ id: null, labels: { en: 'Items' } }]).map((sec) => html`<div>
          <h3>${sec.parent_section_id ? '↳ ' : ''}${label(sec.labels)}</h3>
          ${S.menu.items.filter((it) => !sec.id || it.item.section_id === sec.id).map((it) => {
            const pf = it.facts.find((f) => f.property === 'price.amount');
            const v = it.item.variant;
            const name = label(it.item.labels) + (v ? ` · ${v.label ? label(v.label) : v.size}` : '');
            return html`<div><div class="line"><span class="name">${name}</span><span class="dots"></span><span class="price">${pf?.current_value ? rupees(pf.current_value.amount_minor) : '—'}</span></div>
              <${DishLine} dish=${S.dishById[it.item.dish_id]} k=${'mi:' + it.item.id}/>
              ${it.facts.length ? it.facts.map((f) => html`<${FactLine} f=${f}/>`) : html`<div class="facts">${factChip(null)}</div>`}</div>`;
          })}
        </div>`)}
        ${!S.menu.items.length ? html`<p>No items yet.</p>` : null}
      </div>
      <div class="row"><button disabled=${busy} onClick=${revalidate}>Fetch again with If-None-Match</button>
        ${S.revalidate ? html`<span class="chip" aria-live="polite">${S.revalidate.status === 304 ? '✓ 304 Not Modified · nothing re-sent' : S.revalidate.status}</span>` : null}</div>` : null}
      <${Slip} title="Places request" call=${S.placesCall}/>
      <${Slip} title="Menu request" call=${S.menuCall}/>
      <h3>Duplicates and merges</h3>
      <p class="muted">IDs live forever. A merged place keeps its id and redirects (301) to the survivor; nothing is deleted.</p>
      <button disabled=${busy} onClick=${findMerged}>Find merged places</button>
      ${S.merged ? (S.merged.length ? html`<ul class="list">${S.merged.map((m) => html`<li><p>
        <code>${m.place.id}</code> “${label(m.place.names)}”<br/>
        <span class="chip">↪ merged into</span> <code>${m.place.merged_into}</code><br/>
        <small>GET /v1/places/${m.place.id} → ${m.redirected ? '301, followed to ' + m.finalUrl.replace(/^https?:\/\/[^/]+/, '') : 'not redirected'} · with ?follow=false → 200 and merged_into</small></p></li>`)}</ul>`
        : html`<p class="chip">No merged places</p>`) : null}
    </section>

    <section aria-labelledby="s2b">
      <h2 id="s2b"><span class="n">2</span>Dishes and Wikidata</h2>
      <p class="lead">A dish is a concept shared across places. Its only external link is a Wikidata id; labels, articles and images are read from Wikidata, never copied.</p>
      <button class="primary" disabled=${busy} onClick=${loadDishes}>Show dishes</button>
      ${S.dishes ? html`<ul class="list">${S.dishes.body.items.map((d) => html`<li><p>
        <button class="pick" aria-pressed=${S.dish?.id === d.id} onClick=${() => openDish(d)}><span>${label(d.labels)}${d.labels.ml ? html` · <span lang="ml">${d.labels.ml}</span>` : null}</span><small>${d.kind || ''}</small></button>
        ${d.aliases?.length ? html`<small>also ${d.aliases.map(label).join(', ')}</small>` : null}
        <${Wikidata} link=${d.wikidata}/></p></li>`)}</ul>
        <${Slip} title="Dishes request" call=${S.dishes}/>` : null}
      ${S.dish ? html`<div class="paper"><h3>${label(S.dish.labels)}</h3>
        ${S.dish.wikidata ? html`<${WikiPanel} qid=${S.dish.wikidata.id}/>` : html`<p class="muted">No Wikidata link: nothing to read.</p>`}
        ${S.dishFacts.length ? S.dishFacts.map((f) => html`<${FactLine} f=${f}/>`) : html`<p>No dish-level facts yet.</p>`}</div>` : null}
    </section>

    <section aria-labelledby="s2">
      <h2 id="s2"><span class="n">3</span>What can be observed</h2>
      <p class="lead">A public registry of properties. Each has a scope, a value schema, a half-life and a rule for combining reports.</p>
      ${S.registry ? html`<div class="scroll"><table class="props"><thead><tr><th>Property</th><th>Scope</th><th>Half-life</th><th>Combine</th><th>Who writes</th></tr></thead>
        <tbody>${S.registry.body.items.map((p) => html`<tr><td><code>${p.id}</code>${p.status === 'proposed' ? html` <small>proposed</small>` : null}</td><td>${p.scope}</td><td>${p.half_life_hours < 24 ? p.half_life_hours + ' h' : Math.round(p.half_life_hours / 24) + ' d'}</td><td>${p.aggregation.strategy}</td><td>${p.writers.join(', ')}</td></tr>`)}</tbody></table></div>` : html`<p>Loading…</p>`}
    </section>

    <section aria-labelledby="s3">
      <h2 id="s3"><span class="n">4</span>Your key</h2>
      <p class="lead">No accounts or passwords. Your browser makes an Ed25519 key; every write is signed over its RFC 8785 canonical JSON. The private key never leaves this tab.</p>
      <button class="primary" disabled=${busy || S.edSupported === false} onClick=${makeKey}>${S.keys.length ? 'Make a new key' : 'Make a key and register it'}</button>
      ${S.keys[0] ? html`<dl class="kv">
        <dt>key_id</dt><dd><span class="plate crayon crayon-yellow">${S.keys[0].key_id}</span></dd>
        <dt>public key</dt><dd><code>${S.keys[0].public_key}</code></dd>
        <dt>level</dt><dd>${S.keyCall?.body?.level} · new keys start unverified and weigh less</dd>
      </dl>
      <details class="slip"><summary>The exact bytes that were signed</summary><div class="paper"><pre>${S.keyCall.canonical}</pre></div></details>
      <${Slip} title="Key registration" call=${S.keyCall}/>` : null}
    </section>

    <section aria-labelledby="s4">
      <h2 id="s4"><span class="n">5</span>Add a place, link Wikidata, watch the OSM check</h2>
      <p class="lead">Places, sections and menu items are created with signed writes. Only the place's pin is stored, never where a person is.</p>
      <p class="muted">Names get a “Demo · ” prefix here so demo data stays recognisable.</p>
      <div class="row"><${Field} name="name" label="Name"/><${Field} name="ml" label="Malayalam name (optional)" lang="ml"/></div>
      <div class="row">
        <label>Category<select value=${S.form.category} onChange=${(e) => { S.form.category = e.currentTarget.value; }}>
          ${['tea_stall', 'canteen', 'small_hotel', 'restaurant', 'bakery', 'juice_shop', 'street_cart', 'office_counter', 'cafe', 'other'].map((c) => html`<option value=${c}>${c}</option>`)}
        </select></label>
        <${Field} name="city" label="City"/>
      </div>
      <div class="row"><${Field} name="lat" label="Latitude" inputmode="decimal"/><${Field} name="lng" label="Longitude" inputmode="decimal"/></div>
      <div class="row"><${Field} name="section" label="Section"/><${Field} name="item" label="Menu item"/></div>
      <div class="row"><${Field} name="dish" label="Dish (shared concept, optional)"/><${Field} name="wikidata" label="Wikidata id (optional)" placeholder="Q6097"/>
        <label>Link status<select value=${S.form.wdStatus} onChange=${(e) => { S.form.wdStatus = e.currentTarget.value; }}>
          ${['verified', 'unverified', 'broken'].map((s) => html`<option value=${s}>${s}</option>`)}</select></label></div>
      <p class="muted">Find ids at <a href="https://www.wikidata.org/" rel="noopener noreferrer" target="_blank">wikidata.org ↗</a>, e.g. tea Q6097, masala chai Q877661, pazham pori Q13113151.</p>
      <button class="primary" disabled=${busy || !S.keys.length} onClick=${createPlace}>Sign and create</button>
      ${!S.keys.length ? html`<p class="muted">Make a key first (step 4).</p>` : null}
      <${Problem} call=${S.createCall}/>
      ${S.created?.dish?.wikidata ? html`<p>Dish “${S.created.dish.labels.en}” → <${Wikidata} link=${S.created.dish.wikidata}/></p>` : null}
      ${S.created ? html`<div class="paper"><${PinMap} lat=${S.created.place.location.lat} lng=${S.created.place.location.lng}/>
        ${S.created.dish ? html`<${DishLine} dish=${S.created.dish} k=${'new:' + S.created.dish.id}/>` : null}</div>` : null}
      ${S.created ? html`<div class="paper" aria-live="polite">
        ${S.createdOsm ? html`<${OsmBlock} osm=${S.createdOsm}/>` : html`<p>Waiting for the OSM checker…</p>`}
        ${S.createdOsm?.state === 'unchecked' ? html`<p>The checker searches one pin every 10 seconds, so this can take a moment.</p>` : null}
      </div>
      ${S.createdOsm?.state === 'candidate' ? html`<div class="row"><button disabled=${busy} onClick=${tryLinkOsm}>Confirm the first candidate (try with your key)</button></div>
        <p class="muted">Linking is a person's decision, signed by a verifier or root key. Your new key is not one, so expect 403; see “Demo · Rooftop Cafe” for a place a verifier linked.</p>` : null}
      ${S.osmLinkCall ? html`<p><span class=${'chip' + (S.osmLinkCall.status === 403 ? ' error' : '')}>${S.osmLinkCall.status} ${S.osmLinkCall.body?.title || S.osmLinkCall.body?.status || ''}</span></p><${Slip} title="Admin link_osm request" call=${S.osmLinkCall} open=${true}/>` : null}` : null}
      <${Slip} title="Entity submissions" call=${S.createCall}/>
    </section>

    <section aria-labelledby="s5">
      <h2 id="s5"><span class="n">6</span>Report a price, watch a fact form</h2>
      <p class="lead">An observation is one signed report. A worker combines reports into a fact. No single key can ever confirm a fact: it takes independent people, and trusted keys weigh more.</p>
      <div class="row"><${Field} name="price" label="Price (Rs)" inputmode="decimal"/>
        <button class="primary" disabled=${busy || !S.created} onClick=${reportPrice}>Sign and report</button></div>
      ${!S.created ? html`<p class="muted">Add a place first (step 5).</p>` : null}
      <${Problem} call=${S.priceCall}/>
      ${S.fact ? html`<div class="paper" aria-live="polite">
        <div class="line"><span class="name">${S.created.item.labels.en}</span><span class="dots"></span><span class="price">${rupees(S.fact.current_value.amount_minor)}</span></div>
        <div class="facts">${factChip(S.fact)}</div>
        <pre>${S.factHistory.map((x) => `${x.people} ${x.people === 1 ? 'person ' : 'people '} → ${x.status.padEnd(11)} confidence ${x.confidence.toFixed(2)}`).join('\n')}</pre>
      </div>
      <div class="row">
        <button disabled=${busy || S.fact.status === 'confirmed'} onClick=${anotherPerson}>One more person reports the same price</button>
        <button class="quiet" disabled=${busy} onClick=${resubmit}>Resend the same report</button>
        <button class="quiet" disabled=${busy} onClick=${tamper}>Tamper with it</button>
      </div>
      <p class="muted">Each extra person is a fresh unverified browser key. Unverified keys weigh 0.5, members 1.0; confirmation needs at least 2 people and 60% confidence.</p>
      ${S.dupCall ? html`<h3>Resend → idempotent</h3><${Problem} call=${S.dupCall}/>` : null}
      ${S.tamperCall ? html`<h3>Tampered price → signature fails</h3><${Problem} call=${S.tamperCall}/>` : null}` : null}
      <${Slip} title="Submission" call=${S.priceCall}/>
    </section>

    <section aria-labelledby="s6">
      <h2 id="s6"><span class="n">7</span>What gets published</h2>
      <p class="lead">The signed original stays private. The public observation carries a contributor class and a per-place pseudonym instead of your key.</p>
      <button class="primary" disabled=${busy || !S.priceSent} onClick=${checkPrivacy}>Compare sent and published</button>
      ${S.privacy ? html`<div class="cols">
        <div><h3>You sent</h3><div class="paper"><pre>${pretty({ contributor: S.priceSent.contributor, signature: S.priceSent.signature.slice(0, 24) + '…' })}</pre></div></div>
        <div><h3>The world sees</h3><div class="paper"><pre>${pretty({ contributor: S.privacy.pub?.contributor })}</pre></div></div>
      </div>
      <p aria-live="polite"><span class=${'chip' + (S.privacy.leaks || S.privacy.sigLeak ? ' error' : ' crayon')}>${S.privacy.leaks || S.privacy.sigLeak ? '✕ leak' : '✓'} Your key_id appears ${S.privacy.leaks} times, signatures ${S.privacy.sigLeak ? 'appear' : 'never appear'} in the public response</span></p>
      <${Slip} title="Public observations" call=${S.privacy.call}/>` : null}
    </section>

    <section aria-labelledby="s7">
      <h2 id="s7"><span class="n">8</span>Sync like an app</h2>
      <p class="lead">Apps keep a local copy: download a snapshot, then follow the change feed. Live:</p>
      <div class="feed" aria-live="off">${S.feed.map((c) => html`<div><span>#${c.seq}</span><span>${c.entity_type}</span><code>${c.entity_id}</code><span>${c.op}</span></div>`)}</div>
      <div class="row"><button disabled=${busy} onClick=${loadExport}>Show the latest snapshot</button></div>
      ${S.exportCall?.status === 200 ? html`<p>Snapshot <code>${S.exportCall.body.snapshot}</code> up to change ${S.exportCall.body.changes_seq}, then <code>/v1/changes?after=${S.exportCall.body.changes_seq}</code>.</p>
        <nav class="links">${S.exportCall.body.files.map((f) => html`<a class="outbound" href=${f.url.replace(/^https?:\/\/[^/]+/, BASE)}>${f.name} · ${f.records} ↗</a>`)}</nav>` : null}
      ${S.exportCall && S.exportCall.status !== 200 ? html`<p class="chip">No snapshot yet</p>` : null}
    </section>

    <footer>
      <p>Temporary demo on develop. Data written here is public, permanent and CC0-1.0.</p>
      <p>Code AGPL-3.0-or-later · data CC0-1.0 · fonts SIL OFL 1.1.</p>
    </footer>
  </main>`;
}

const root = document.getElementById('app');
function draw() { render(html`<${App}/>`, root); }
draw();
status().then(() => { search('city'); pollFeed(); });
