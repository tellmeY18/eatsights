// Eatsights live demo. Temporary, develop only; not part of the product (see README.md).
import { h, render } from '/demo/vendor/preact.module.js';
import htm from '/demo/vendor/htm.module.js';
import * as es from '/demo/eatsights.js';

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
  form: { name: 'My test stall', ml: '', category: 'tea_stall', city: 'ernakulam', lat: '9.9816', lng: '76.2999', section: 'Drinks', item: 'Tea', price: '15' },
  created: null, createCall: null,
  priceCall: null, priceSent: null, fact: null, factHistory: [], busy: false,
  dupCall: null, tamperCall: null,
  privacy: null,
  feed: [], feedAfter: null, exportCall: null,
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
  if (!k) throw new Error('make a key first (step 3)');
  const f = S.form;
  const lat = Number(f.lat), lng = Number(f.lng);
  if (!f.name.trim() || !Number.isFinite(lat) || !Number.isFinite(lng)) throw new Error('name, lat and lng are required');
  const place = {
    id: es.ulid('pl_'), names: { en: 'Demo · ' + f.name.trim().replace(/^Demo · /, ''), ...(f.ml.trim() ? { ml: f.ml.trim() } : {}) },
    category: f.category, location: { lat, lng, precision_m: 25 }, city: f.city.trim() || 'ernakulam',
  };
  const section = { id: es.ulid('sc_'), place_id: place.id, labels: { en: f.section.trim() || 'Menu' }, kind: 'other', sort: 1 };
  const item = { id: es.ulid('mi_'), place_id: place.id, section_id: section.id, labels: { en: f.item.trim() || 'Item' } };
  const subs = [];
  for (const [t, e] of [['place', place], ['section', section], ['menu_item', item]]) subs.push((await es.entitySubmission(k, t, e)).signed);
  const r = await api.post('/v1/entities', subs);
  set({ createCall: r, created: { place, section, item }, fact: null, factHistory: [], priceCall: null, dupCall: null, tamperCall: null, privacy: null });
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
  if (!S.created) throw new Error('add a place first (step 4)');
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
        ${S.menu.place_facts?.length ? html`<h3>About the place</h3>${S.menu.place_facts.map((f) => html`<${FactLine} f=${f}/>`)}` : null}
        ${(S.menu.sections.length ? S.menu.sections : [{ id: null, labels: { en: 'Items' } }]).map((sec) => html`<div>
          <h3>${sec.parent_section_id ? '↳ ' : ''}${label(sec.labels)}</h3>
          ${S.menu.items.filter((it) => !sec.id || it.item.section_id === sec.id).map((it) => {
            const pf = it.facts.find((f) => f.property === 'price.amount');
            const v = it.item.variant;
            const name = label(it.item.labels) + (v ? ` · ${v.label ? label(v.label) : v.size}` : '');
            return html`<div><div class="line"><span class="name">${name}</span><span class="dots"></span><span class="price">${pf?.current_value ? rupees(pf.current_value.amount_minor) : '—'}</span></div>
              ${it.facts.length ? it.facts.map((f) => html`<${FactLine} f=${f}/>`) : html`<div class="facts">${factChip(null)}</div>`}</div>`;
          })}
        </div>`)}
        ${!S.menu.items.length ? html`<p>No items yet.</p>` : null}
      </div>
      <div class="row"><button disabled=${busy} onClick=${revalidate}>Fetch again with If-None-Match</button>
        ${S.revalidate ? html`<span class="chip" aria-live="polite">${S.revalidate.status === 304 ? '✓ 304 Not Modified · nothing re-sent' : S.revalidate.status}</span>` : null}</div>` : null}
      <${Slip} title="Places request" call=${S.placesCall}/>
      <${Slip} title="Menu request" call=${S.menuCall}/>
    </section>

    <section aria-labelledby="s2">
      <h2 id="s2"><span class="n">2</span>What can be observed</h2>
      <p class="lead">A public registry of properties. Each has a scope, a value schema, a half-life and a rule for combining reports.</p>
      ${S.registry ? html`<div class="scroll"><table class="props"><thead><tr><th>Property</th><th>Scope</th><th>Half-life</th><th>Combine</th><th>Who writes</th></tr></thead>
        <tbody>${S.registry.body.items.map((p) => html`<tr><td><code>${p.id}</code>${p.status === 'proposed' ? html` <small>proposed</small>` : null}</td><td>${p.scope}</td><td>${p.half_life_hours < 24 ? p.half_life_hours + ' h' : Math.round(p.half_life_hours / 24) + ' d'}</td><td>${p.aggregation.strategy}</td><td>${p.writers.join(', ')}</td></tr>`)}</tbody></table></div>` : html`<p>Loading…</p>`}
    </section>

    <section aria-labelledby="s3">
      <h2 id="s3"><span class="n">3</span>Your key</h2>
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
      <h2 id="s4"><span class="n">4</span>Add a place</h2>
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
      <button class="primary" disabled=${busy || !S.keys.length} onClick=${createPlace}>Sign and create</button>
      ${!S.keys.length ? html`<p class="muted">Make a key first (step 3).</p>` : null}
      <${Problem} call=${S.createCall}/>
      <${Slip} title="Entity submissions" call=${S.createCall}/>
    </section>

    <section aria-labelledby="s5">
      <h2 id="s5"><span class="n">5</span>Report a price, watch a fact form</h2>
      <p class="lead">An observation is one signed report. A worker combines reports into a fact. No single key can ever confirm a fact: it takes independent people, and trusted keys weigh more.</p>
      <div class="row"><${Field} name="price" label="Price (Rs)" inputmode="decimal"/>
        <button class="primary" disabled=${busy || !S.created} onClick=${reportPrice}>Sign and report</button></div>
      ${!S.created ? html`<p class="muted">Add a place first (step 4).</p>` : null}
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
      <h2 id="s6"><span class="n">6</span>What gets published</h2>
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
      <h2 id="s7"><span class="n">7</span>Sync like an app</h2>
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
