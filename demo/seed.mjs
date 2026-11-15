// Seed develop with DEMO data that exercises every edge case the API handles.
// Every place is fictional and named "Demo · …". Writes public, permanent CC0 data.
//
//   node demo/seed.mjs <base-url> <root-private-key>
//   (develop: ROOT=$(jq -r .private_key .dev/root.json); node demo/seed.mjs https://develop.letsimpl.fyi "$ROOT")
import * as es from './eatsights.js';

const [base = 'https://develop.letsimpl.fyi', rootSeed] = process.argv.slice(2);
if (!rootSeed) { console.error('usage: node demo/seed.mjs <base> <root-private-key>'); process.exit(2); }
const api = es.client(base);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const ago = (h) => new Date(Date.now() - h * 3600e3).toISOString().replace(/\.\d{3}Z$/, 'Z');
const die = (m, x) => { console.error('FAIL', m, x ? JSON.stringify(x).slice(0, 600) : ''); process.exit(1); };
const D = (s) => `Demo · ${s}`;

async function expectBatch(path, items, label) {
  const r = await api.post(path, items);
  const bad = (r.body?.results || []).filter((x) => !['accepted', 'duplicate'].includes(x.status));
  if (r.status !== 200 || bad.length) die(label, r.body);
  return r.body.results;
}

// ---- keys
async function key(label) {
  const k = await es.generateKey();
  const { signed } = await es.keyRegistration(k);
  const r = await api.post('/v1/keys', signed);
  if (r.status >= 300) die(`register ${label}`, r.body);
  k.label = label;
  return k;
}
async function attest(issuer, fields) {
  const { signed } = await es.sign(issuer, { id: es.ulid('at_'), issuer_key_id: issuer.key_id, issued_at: es.nowUtc(), ...fields });
  await expectBatch('/v1/attestations', [signed], `attestation ${fields.type}`);
  return signed;
}
async function admin(root, action, params) {
  const { signed } = await es.sign(root, { id: es.ulid('es_'), action, params, issued_at: es.nowUtc(), issuer_key_id: root.key_id });
  const path = action === 'remove_observation' ? '/v1/admin/removals' : '/v1/admin/merges';
  const r = await api.post(path, signed);
  if (r.body?.status !== 'accepted') die(`admin ${action}`, r.body);
  return r.body;
}

// ---- entities
async function entities(k, list) {
  const subs = [];
  for (const [t, e] of list) subs.push((await es.entitySubmission(k, t, e)).signed);
  for (let i = 0; i < subs.length; i += 100) await expectBatch('/v1/entities', subs.slice(i, i + 100), 'entities');
}

// ---- observations
const queue = [];
async function obs(k, subject, property, value, { hoursAgo = 0, kind = 'value', refers_to, source = 'customer', method } = {}) {
  const o = {
    id: es.ulid('ob_'), schema_version: '0.3', kind, property, subject,
    observed_at: ago(hoursAgo), submitted_at: es.nowUtc(),
    method: method || (source === 'vendor' ? 'vendor_declared' : source === 'import' ? 'import' : 'in_person'),
    source_type: source, prompt_type: 'unprompted',
    contributor: { key_id: k.key_id, checkin_verified: source === 'customer' }, license: 'CC0-1.0',
  };
  if (value !== undefined) o.value = value;
  if (refers_to) o.refers_to = refers_to;
  if (source !== 'customer') delete o.prompt_type;
  const { signed } = await es.sign(k, o);
  queue.push(signed);
  return signed;
}
async function flush(label = 'observations') {
  while (queue.length) await expectBatch('/v1/submissions', queue.splice(0, 100), label);
}
const price = (rs, extra = {}) => ({ amount_minor: Math.round(rs * 100), currency: 'INR', fulfilment: 'dine_in', ...extra });

// ---------------------------------------------------------------------------
const root = await es.importSeed(rootSeed);
console.log('root', root.key_id);

const [m1, m2, m3, m4, m5] = await Promise.all(['m1', 'm2', 'm3', 'm4', 'm5'].map(key));
const [u1, u2] = await Promise.all(['u1', 'u2'].map(key));
const [ver, vend, imp] = await Promise.all(['verifier', 'vendor', 'import'].map(key));
for (const m of [m1, m2, m3, m4, m5]) await attest(root, { type: 'vouch', subject_key_id: m.key_id, level_granted: 'member' });
await attest(root, { type: 'vouch', subject_key_id: m5.key_id, level_granted: 'trusted' });
await attest(root, { type: 'vouch', subject_key_id: ver.key_id, level_granted: 'verifier' });
console.log('keys: 5 members (1 trusted), 2 unverified, 1 verifier, 1 vendor, 1 import');

// Places, all fictional. Pins are spread over Kochi, Thrissur and Kozhikode.
const P = (name, ml, category, city, lat, lng) => ({
  id: es.ulid('pl_'), names: { en: D(name), ...(ml ? { ml } : {}) }, category, city,
  location: { lat, lng, precision_m: 20 },
});
const kada = P('Kaloor Chaya Kada', 'ഡെമോ · കലൂർ ചായക്കട', 'tea_stall', 'ernakulam', 9.9971, 76.3007);
const meals = P('Station Meals Hotel', 'ഡെമോ · സ്റ്റേഷൻ ഊണ് ഹോട്ടൽ', 'small_hotel', 'ernakulam', 9.9696, 76.2911);
const juice = P('Marine Drive Juice Corner', null, 'juice_shop', 'ernakulam', 9.9819, 76.2763);
const cafe = P('Rooftop Cafe', null, 'cafe', 'ernakulam', 9.9640, 76.2427);
const cart = P('Empty Cart', null, 'street_cart', 'ernakulam', 10.0159, 76.3419);
const canteen = P('Campus Canteen', 'ഡെമോ · കാമ്പസ് കാന്റീൻ', 'canteen', 'thrissur', 10.5276, 76.2144);
const bakery = P('Beach Bakery', null, 'bakery', 'kozhikode', 11.2588, 75.7804);
const dupe = { ...P('Kaloor Chaya Kada', null, 'tea_stall', 'ernakulam', 9.9972, 76.3008) };

// Dishes (shared across places) with dish-level facts.
const dTea = { id: es.ulid('di_'), labels: { en: D('Tea'), ml: 'ചായ' }, kind: 'beverage' };
const dPori = { id: es.ulid('di_'), labels: { en: D('Pazham pori'), ml: 'പഴംപൊരി' }, kind: 'snack' };
const dMeals = { id: es.ulid('di_'), labels: { en: D('Kerala meals'), ml: 'ഊണ്' }, kind: 'dish' };

const S = (place, en, kind, sort, parent) => ({ id: es.ulid('sc_'), place_id: place.id, labels: { en }, kind, sort, ...(parent ? { parent_section_id: parent.id } : {}) });
const I = (place, sec, en, ml, extra = {}) => ({ id: es.ulid('mi_'), place_id: place.id, section_id: sec.id, labels: { en, ...(ml ? { ml } : {}) }, ...extra });

const kDrinks = S(kada, 'Drinks', 'drinks', 1);
const kHot = S(kada, 'Hot drinks', 'drinks', 1, kDrinks); // nested section
const kSnacks = S(kada, 'Snacks', 'snacks', 2);
const tea = I(kada, kHot, 'Tea', 'ചായ', { dish_id: dTea.id, variant: { size: 'regular' } });
const teaL = I(kada, kHot, 'Tea', 'ചായ', { dish_id: dTea.id, variant: { size: 'large' } });
const coffee = I(kada, kHot, 'Filter coffee', 'കാപ്പി');
const pori = I(kada, kSnacks, 'Pazham pori', 'പഴംപൊരി', { dish_id: dPori.id });
const vada = I(kada, kSnacks, 'Uzhunnu vada', 'ഉഴുന്നുവട');

const mMain = S(meals, 'Meals', 'meals', 1);
const thali = I(meals, mMain, 'Kerala meals (thali)', 'ഊണ്', { dish_id: dMeals.id, variant: { size: 'full' } });
const fishMeals = I(meals, mMain, 'Fish curry meals', 'മീൻ കറി ഊണ്');
const biryani = I(meals, mMain, 'Chicken biryani', null);
const mDrinks = S(meals, 'Drinks', 'drinks', 2);
const mango = I(meals, mDrinks, 'Mango juice', null);

const jMain = S(juice, 'Juices', 'drinks', 1);
const lime = I(juice, jMain, 'Fresh lime soda', null);
const cMain = S(cafe, 'Coffee', 'drinks', 1);
const cappu = I(cafe, cMain, 'Cappuccino', null);
const caMain = S(canteen, 'Counter', 'meals', 1);
const porotta = I(canteen, caMain, 'Porotta + egg curry', 'പൊറോട്ട');
const caTea = I(canteen, caMain, 'Tea', 'ചായ', { dish_id: dTea.id });
const bMain = S(bakery, 'Bakes', 'snacks', 1);
const puffs = I(bakery, bMain, 'Egg puffs', null);
const halwa = I(bakery, bMain, 'Kozhikodan halwa', 'ഹൽവ', { variant: { size: 'half', label: { en: '250 g' } } });

await entities(m1, [
  ['place', kada], ['place', meals], ['place', juice], ['place', cafe], ['place', cart], ['place', canteen], ['place', bakery],
  ['dish', dTea], ['dish', dPori], ['dish', dMeals],
  ...[kDrinks, kHot, kSnacks, mMain, mDrinks, jMain, cMain, caMain, bMain].map((s) => ['section', s]),
  ...[tea, teaL, coffee, pori, vada, thali, fishMeals, biryani, mango, lime, cappu, porotta, caTea, puffs, halwa].map((i) => ['menu_item', i]),
]);
await sleep(1100); // the duplicate is created later, so the original is the older record and survives the merge
await entities(u1, [['place', dupe]]);
console.log('entities: 8 places (1 duplicate), 3 dishes, 9 sections (1 nested), 15 menu items (variants)');

const sp = (p) => ({ scope: 'place', place_id: p.id });
const sm = (i) => ({ scope: 'menu_item', place_id: i.place_id, menu_item_id: i.id });
const sd = (d) => ({ scope: 'dish', dish_id: d.id });

// Vendor: a verifier binds the vendor key to the tea stall (public vendor_of attestation).
await attest(ver, { type: 'vendor_of', subject_key_id: vend.key_id, place_id: kada.id });

// ---- Kaloor Chaya Kada: the full set
const teaA = await obs(m1, sm(tea), 'price.amount', price(15), { hoursAgo: 3 });
await obs(m2, sm(tea), 'price.amount', price(15), { hoursAgo: 1 });                  // confirmed: 2 members agree
await obs(m4, sm(tea), 'price.amount', undefined, { kind: 'confirm', refers_to: teaA.id }); // a "still Rs 15" confirmation
await obs(vend, sm(tea), 'price.amount', price(12), { source: 'vendor' });            // vendor says 12: vendor_value_differs
await obs(m1, sm(teaL), 'price.amount', price(20));
await obs(m3, sm(teaL), 'price.amount', price(20));
const cofA = await obs(m1, sm(coffee), 'price.amount', price(20), { hoursAgo: 2 });
await obs(m2, sm(coffee), 'price.amount', price(30), { hoursAgo: 1 });                // contested: members disagree
await obs(m5, sm(coffee), 'price.amount', undefined, { kind: 'dispute', refers_to: cofA.id });
await obs(u1, sm(pori), 'price.amount', price(12));                                  // unconfirmed: one unverified person
await obs(m3, sm(vada), 'price.amount', price(10), { hoursAgo: 24 });
await obs(m4, sm(vada), 'price.amount', price(10), { hoursAgo: 20 });
await obs(m3, sm(vada), 'availability.observed', { state: 'sold_out' });              // sold out right now
await obs(m3, sm(vada), 'availability.sold_out_time', { local_time: '10:30', day_type: 'weekday' }, { hoursAgo: 48 });
await obs(m4, sm(vada), 'availability.sold_out_time', { local_time: '11:00', day_type: 'weekday' }, { hoursAgo: 24 });
await obs(m5, sm(vada), 'availability.sold_out_time', { local_time: '10:45', day_type: 'weekday' });
await obs(vend, sm(vada), 'availability.declared_schedule', { rule: 'Mo-Sa 06:00-11:30' }, { source: 'vendor' });
for (const [k, s] of [[m1, 240], [m2, 300], [m3, 420]]) await obs(k, sm(tea), 'prep.wait_time', { seconds: s, stage: 'order_to_served' }); // median 300 s
for (const [k, s] of [[m1, 4], [m2, 5], [m4, 4]]) await obs(k, sm(tea), 'quality.score', { score: s, worth_price: true });
await obs(m1, sm(tea), 'composition.milk', { type: 'cow' });
await obs(m2, sm(tea), 'composition.milk', { type: 'cow' });
await obs(u2, sm(tea), 'composition.milk', { type: 'powdered' });                     // mode: cow wins
await obs(m1, sm(tea), 'composition.sweetness', { level: 3 });
await obs(m2, sm(tea), 'composition.sweetness', { level: 2 });
await obs(m1, sm(tea), 'customisation.options', { options: ['less_sugar', 'extra_strong'] });
await obs(m2, sm(tea), 'customisation.options', { options: ['no_sugar', 'add_ginger'] }); // union
await obs(m1, sm(tea), 'serving.temperature', { state: 'hot' });
await obs(m2, sm(tea), 'serving.quantity', { value: 120, unit: 'ml' });
await obs(m3, sm(tea), 'serving.quantity', { value: 150, unit: 'ml' });
await obs(m1, sp(kada), 'payment.methods', { methods: ['cash', 'upi'] });
await obs(m2, sp(kada), 'payment.methods', { methods: ['upi', 'card'] });             // union: cash, upi, card
await obs(m1, sp(kada), 'seating.kind', { kinds: ['standing', 'bench'] });
await obs(m1, sp(kada), 'status.operating', { state: 'active' });
await obs(m2, sp(kada), 'status.operating', { state: 'active' });
await obs(vend, sp(kada), 'hours.declared', { rule: 'Mo-Sa 06:00-20:00; Su off' }, { source: 'vendor' });
await obs(m4, sp(kada), 'hours.observed_open', { state: 'open_now' });
await obs(m4, sp(kada), 'queue.length', { bucket: '1_to_3' });
await obs(m1, sd(dTea), 'dish.diet', { diet: 'veg' });
await obs(m2, sd(dTea), 'dish.diet', { diet: 'veg' });
await obs(m1, sd(dTea), 'dish.allergens', { allergens: ['milk'] });
await obs(m1, sd(dPori), 'dish.diet', { diet: 'vegan' });
await obs(m2, sd(dPori), 'dish.allergens', { allergens: ['gluten'] });

// ---- Station Meals Hotel: thali, time-based price, tax, packaging, delisted and seasonal
await obs(m1, sm(thali), 'price.amount', price(90, { tax_included: true }));
await obs(m2, sm(thali), 'price.amount', price(90, { tax_included: true }));
await obs(m3, sm(thali), 'price.amount', price(70, { tax_included: true, valid_during: 'Mo-Fr 12:00-15:00' })); // lunch-hour price
await obs(m1, sm(thali), 'includes.items', { items: ['rice', 'sambar', 'curry', 'pickle', 'papad', 'unlimited_refill'] });
await obs(m2, sm(thali), 'includes.items', { items: ['rice', 'sambar', 'curry', 'pickle', 'papad', 'buttermilk', 'unlimited_refill'] });
await obs(m1, sm(fishMeals), 'price.amount', price(140));
await obs(m4, sm(fishMeals), 'price.amount', price(140));
await obs(m1, sm(fishMeals), 'price.packaging_charge', { amount_minor: 1000, currency: 'INR' });
await obs(m1, sm(fishMeals), 'price.amount', { amount_minor: 15000, currency: 'INR', fulfilment: 'takeaway' });
await obs(m1, sm(biryani), 'price.amount', price(160), { hoursAgo: 72 });
await obs(m2, sm(biryani), 'listing.status', { state: 'delisted' });                  // no longer on the menu
await obs(m3, sm(biryani), 'listing.status', { state: 'delisted' });
await obs(m2, sm(mango), 'listing.status', { state: 'seasonal' });
await obs(m2, sm(mango), 'price.amount', price(50));
await obs(m1, sp(meals), 'payment.methods', { methods: ['cash', 'upi'] });
await obs(m1, sp(meals), 'seating.kind', { kinds: ['table', 'family_section', 'air_conditioned'] });

// ---- Juice corner: temporarily closed, import-class price
await obs(imp, sm(lime), 'price.amount', price(30), { source: 'import' });             // import class, lower weight
await obs(m1, sp(juice), 'status.operating', { state: 'temporarily_closed' });
await obs(m2, sp(juice), 'status.operating', { state: 'temporarily_closed' });

// ---- Rooftop cafe: prices last seen 25 days ago (old but within the 30-day window)
await obs(m1, sm(cappu), 'price.amount', price(180), { hoursAgo: 25 * 24 });
await obs(m2, sm(cappu), 'price.amount', price(180), { hoursAgo: 24 * 24 });

// ---- Campus canteen: fast-decaying facts go stale
await obs(m1, sm(porotta), 'price.amount', price(45));
await obs(m2, sm(porotta), 'price.amount', price(45));
await obs(m1, sm(porotta), 'availability.observed', { state: 'available' }, { hoursAgo: 10 }); // half-life 2 h: stale
await obs(m1, sp(canteen), 'queue.length', { bucket: 'over_10' }, { hoursAgo: 3 });            // half-life 0.5 h: stale
await obs(m2, sp(canteen), 'hours.observed_open', { state: 'closed_now' });
await obs(m3, sm(caTea), 'price.amount', price(10));
await obs(m4, sm(caTea), 'price.amount', price(12));
await obs(m5, sm(caTea), 'price.amount', price(10));                                    // trusted key tips it: 10 wins

// ---- Beach bakery: one observation later removed for a legal reason
await obs(m1, sm(puffs), 'price.amount', price(20));
await obs(m2, sm(puffs), 'price.amount', price(20));
const removed = await obs(u2, sm(halwa), 'price.amount', price(400));
await obs(m3, sm(halwa), 'price.amount', price(220));

// ---- the duplicate stall gets one report before it is merged
await obs(u1, sp(dupe), 'status.operating', { state: 'active' });

await flush();
console.log('observations accepted');

// Admin (root-signed): merge the duplicate into the original, and a legal removal.
await sleep(1500);
const merge = await admin(root, 'merge', { a_id: dupe.id, b_id: kada.id });
console.log('merged', merge.result?.merged, '->', merge.result?.merged_into);
await admin(root, 'remove_observation', { observation_id: removed.id, reason: 'legal' });
console.log('removed observation', removed.id);

// ---- wait for the worker, then print what each case became
await sleep(4000);
const facts = async (q) => (await api.get('/v1/facts?limit=200&' + q)).body?.items || [];
const show = async (label, q, prop) => {
  let f;
  for (let i = 0; i < 20; i++) { f = (await facts(q)).find((x) => x.property === prop); if (f) break; await sleep(500); }
  console.log(label.padEnd(44), f ? `${f.status.padEnd(11)} ${JSON.stringify(f.current_value)}${f.vendor_value_differs ? '  vendor differs' : ''}` : 'no fact');
};
await show('Tea price (2 members + vendor)', `menu_item_id=${tea.id}`, 'price.amount');
await show('Coffee price (members disagree + dispute)', `menu_item_id=${coffee.id}`, 'price.amount');
await show('Pazham pori price (1 unverified)', `menu_item_id=${pori.id}`, 'price.amount');
await show('Vada availability (sold out)', `menu_item_id=${vada.id}`, 'availability.observed');
await show('Vada sold-out time (median)', `menu_item_id=${vada.id}`, 'availability.sold_out_time');
await show('Tea wait time (median)', `menu_item_id=${tea.id}`, 'prep.wait_time');
await show('Tea milk (mode)', `menu_item_id=${tea.id}`, 'composition.milk');
await show('Tea options (union)', `menu_item_id=${tea.id}`, 'customisation.options');
await show('Stall payments (union)', `place_id=${kada.id}`, 'payment.methods');
await show('Thali includes (union)', `menu_item_id=${thali.id}`, 'includes.items');
await show('Biryani listing (delisted)', `menu_item_id=${biryani.id}`, 'listing.status');
await show('Juice corner status', `place_id=${juice.id}`, 'status.operating');
await show('Lime soda price (import only)', `menu_item_id=${lime.id}`, 'price.amount');
await show('Canteen porotta availability (10 h old)', `menu_item_id=${porotta.id}`, 'availability.observed');
await show('Canteen queue (3 h old)', `place_id=${canteen.id}`, 'queue.length');
await show('Canteen tea (trusted tips 10 vs 12)', `menu_item_id=${caTea.id}`, 'price.amount');
await show('Tea dish diet (dish scope)', `dish_id=${dTea.id}`, 'dish.diet');
const redirect = await fetch(`${base}/v1/places/${dupe.id}`, { redirect: 'manual' });
console.log('Duplicate place'.padEnd(44), redirect.status, redirect.headers.get('location'));
console.log('done. Places:', [kada, meals, juice, cafe, cart, canteen, bakery].map((p) => `${p.names.en} ${p.id}`).join('\n  '));
