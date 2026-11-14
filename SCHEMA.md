# Schema draft v0.3

Project: Eatsights. Data licence: **CC0 1.0**. BDFL: Vysakh Premkumar (see `GOVERNANCE.md`).

Status: draft. JSON syntax, `$ref` targets and registry consistency were checked, but the schema has **not** been run through a full JSON Schema 2020-12 validator. Do that before relying on it. `unevaluatedProperties` (used on the two observation forms) needs a 2020-12 validator.

## What changed from v0.2

| v0.2 | v0.3 |
|---|---|
| `place` > `menu` > `section` > `menu_item` | `place` > `section` (nestable) > `menu_item`. No `menu` entity. |
| Menu validity window | Timing is a property: `availability.declared_schedule` (exact) and `availability.meal_times` (coarse) |
| OSM link: linked / candidate / unmapped | Four states: `unchecked`, `unmapped` (with `checked_at`), `candidate`, `linked`. Plus a system-set `ready_to_map` flag |
| No duplicate handling | `record_state` and `merged_into` on places (and menu items). IDs are never deleted |
| `claimed_by_vendor` flag on place | Removed. Vendor ownership is derived from `vendor_of` attestations |
| One observation shape with `key_id` | Two forms: private `submission` (key + signature) and public `observation` (class + per-place pseudonym) |
| No trust model | `key` and `attestation` records: a rooted trust chain with levels, vouching, vendor binding, revocation and rotation |
| Combos undecided | A combo or thali is one menu item with `includes.items` |

## Structure

```
place            own ID; optional OSM parent link; may be merged into another place
 └─ section      nestable (Drinks > Hot drinks). The place's sections are its menu.
     └─ menu_item   place-specific variant, e.g. "Tea, regular"
          └─ dish   shared concept; the only external link is Wikidata
```

Everything that changes over time is an observation on a subject at scope `place`, `menu_item` or `dish`. The registry (`properties.json`, 30 properties) defines what can be observed.

## Linking, not copying

| Source | Stored | Read at use time |
|---|---|---|
| OSM | `place.osm` (state, ID, dates) | Everything about the object |
| Wikidata | `dish.wikidata` (rarely `place.wikidata`) | Labels, Wikipedia article, Commons category and image |

We store no images. Dish images come through Wikidata and Commons. Place photos belong with the OSM object. Menu photos used by an extractor are processed and discarded.

## Adding a place that isn't in OSM

1. **Add.** Contributor drops a pin, names the place, picks a category. The app first shows nearby existing places and OSM candidates ("is it one of these?"). State starts at `unchecked`.
2. **Check OSM.** A job (or the app) searches around the pin. Nothing found: `unmapped` with `checked_at`. Possible matches: `candidate` with the list.
3. **Confirm.** A person confirms a candidate and the state becomes `linked`, storing only the OSM ID.
4. **Ready to map.** An unmapped place gets `ready_to_map: true` once it has a name, category and pin, and its existence is confirmed by independent contributors (`status.operating` confirmed). These feed an export queue for OSM mappers, with suggested OSM tags generated at export time (name, `name:ml`, amenity, opening hours). Tags are not stored in the schema.
5. **Re-check.** A periodic job re-searches `unmapped` places. A new match moves them to `candidate`.

Our data can go into OSM (CC0 is compatible). Data from OSM must never come in. Easier mapping flows (Notes, MapRoulette tasks) are app features, not schema.

## Duplicate places

IDs live forever. A merge sets the loser to `record_state: merged` with `merged_into: <survivor>`. Observations stay attached and the aggregation job follows the redirect. The survivor is the record with an OSM link, otherwise the older one. Detection is automatic (same category, close pin, similar name); confirmation is a person. A wrong merge is undone by clearing the redirect, since nothing was deleted.

## Availability: how to combine the layers

Layers: `listing.status` (is it on the menu), `availability.declared_schedule` / `availability.meal_times` (when the vendor says), `availability.observed` / `availability.sold_out_time` (what customers saw). Check in this order:

1. **Place hours are a hard limit.** `hours.declared`, overridden by fresh `hours.observed_open`. Closed place means nothing is available.
2. **Listed?** `listing.status` must be `listed` or `seasonal`.
3. **Declared schedule.** Use the exact schedule if present; otherwise fall back to `meal_times`; otherwise assume the item follows place hours.
4. **Fresh observation wins.** A recent `availability.observed` beats any schedule. Its 2-hour half-life means it fades quickly.
5. `price.valid_during` selects which price applies. It never affects availability.

## Observations: two forms

**Submission (private).** Sent by a client. Includes `contributor.key_id` and a required Ed25519 `signature` over the canonical JSON of every other field. The write API verifies the signature, checks the key's status, applies rate limits, and stores the submission on the write side only.

**Observation (public).** What gets published. The raw key and signature are removed and replaced with:

- `class`: `unverified`, `member`, `trusted`, `verifier`, `vendor` or `import`. `vendor` applies only when the key holds a `vendor_of` attestation for that place.
- `pseudonym`: `HMAC(secret_salt, key_id + anchor)` truncated to 22 characters, where anchor is `place_id` for place and menu-item scope, or `dish_id` for dish scope. Consumers can count distinct contributors per place, but cannot link one person's activity across places. The salt is secret and held by the write side.

Validation (unchanged idea, one more step):

1. Check the envelope against this schema.
2. Verify the signature and the key's status.
3. Look up `property` in the registry; check scope, writer and `value_schema`.

## Trust chain

Levels, from the root: `root` (BDFL key, depth 0), `verifier` (may vouch others and bind vendors), `trusted`, `member`, `unverified` (default for new keys).

- **Vouch** (`type: vouch`): a higher-level key signs an attestation granting `member`, `trusted` or `verifier` to another key. The app's QR flow produces this. The issuer must hold a higher level than the one granted (verifier-granting is root only, in the draft policy).
- **Vendor binding** (`type: vendor_of`): a verifier, after physically checking the shop, binds a key to one `place_id`. Public. This replaces the old `claimed_by_vendor` flag.
- **Revoke** (`type: revoke`): cancels an earlier attestation via `revokes`. The system also lowers the standing of whoever vouched for an abusive key, so vouching has a cost.
- **Rotate** (`type: rotate`): the old key vouches for its replacement (new phone). The old key becomes `rotated` with `successor_key_id`, and reputation carries over.
- **Weights, not powers.** Trust raises an observation's weight. It never lets a single key confirm a fact alone: `confirmed` still needs independent contributors.
- **Depth.** `key.depth` counts steps from the root. A maximum depth is a policy choice (open question).
- **Expiry.** `expires_at` is optional. Short expiry on vendor bindings is sensible.

Privacy split:

| Public | Private (write side only) |
|---|---|
| Observations with class and pseudonym | Raw `key_id` and signatures on submissions |
| `vendor_of` attestations | `vouch`, `revoke`, `rotate` attestations (the "who knows whom" graph) |
| Root and verifier keys | All other key records |

Protect the root private key: keep it offline or on a hardware key, and name a backup root in `GOVERNANCE.md`.

## Examples

A place that nobody has checked against OSM yet:

```json
{
  "id": "pl_01JC8Y0A1B2C3D4E5F6G7H8J9K",
  "names": { "en": "Kaloor Corner Tea Stall", "ml": "കലൂർ കോർണർ ചായക്കട" },
  "category": "tea_stall",
  "location": { "lat": 9.9942, "lng": 76.3012, "precision_m": 15 },
  "city": "ernakulam",
  "admin": { "state": "Kerala", "district": "Ernakulam" },
  "osm": { "state": "unchecked" },
  "record_state": "active",
  "created_at": "2026-11-14T08:00:00Z"
}
```

The same place after a search found nothing, and enough people confirmed it exists:

```json
"osm": { "state": "unmapped", "checked_at": "2026-11-20T04:00:00Z", "ready_to_map": true }
```

A submission (private):

```json
{
  "id": "ob_01JC8Z3QK5M7N2P4R6T8V0X1Y2",
  "schema_version": "0.3",
  "kind": "value",
  "property": "price.amount",
  "subject": { "scope": "menu_item", "place_id": "pl_01JC8Y0A1B2C3D4E5F6G7H8J9K", "menu_item_id": "mi_01JC8Y5M2N3P4Q5R6S7T8V9W0X" },
  "value": { "amount_minor": 1000, "currency": "INR", "fulfilment": "dine_in", "tax_included": true },
  "observed_at": "2026-11-14T09:12:00Z",
  "submitted_at": "2026-11-14T09:14:31Z",
  "method": "receipt",
  "source_type": "customer",
  "prompt_type": "unprompted",
  "contributor": { "key_id": "k7Qm2xWv9LpR4tYb8NcD3a", "checkin_verified": true },
  "signature": "<86 characters of base64url>",
  "license": "CC0-1.0"
}
```

The published form of the same observation:

```json
{
  "id": "ob_01JC8Z3QK5M7N2P4R6T8V0X1Y2",
  "schema_version": "0.3",
  "kind": "value",
  "property": "price.amount",
  "subject": { "scope": "menu_item", "place_id": "pl_01JC8Y0A1B2C3D4E5F6G7H8J9K", "menu_item_id": "mi_01JC8Y5M2N3P4Q5R6S7T8V9W0X" },
  "value": { "amount_minor": 1000, "currency": "INR", "fulfilment": "dine_in", "tax_included": true },
  "observed_at": "2026-11-14T09:12:00Z",
  "submitted_at": "2026-11-14T09:14:31Z",
  "method": "receipt",
  "source_type": "customer",
  "prompt_type": "unprompted",
  "contributor": { "class": "member", "pseudonym": "Zq8nT3vLp0XhYc5JmR2sWd", "checkin_verified": true },
  "license": "CC0-1.0"
}
```

A verifier binding a key to a stall (public):

```json
{
  "id": "at_01JC8Z9HJ1K3M5N7P9Q1R3S5T7",
  "type": "vendor_of",
  "issuer_key_id": "r0Ot8KeYvErIfIeRaBcDeF",
  "subject_key_id": "v2Hs8DfG1kLp5QwEr9TyUi",
  "place_id": "pl_01JC8Y0A1B2C3D4E5F6G7H8J9K",
  "issued_at": "2026-11-14T07:00:00Z",
  "expires_at": "2027-11-14T07:00:00Z",
  "signature": "<86 characters of base64url>"
}
```

## Open questions

1. **Trust policy numbers:** weight per level, maximum chain depth, default attestation expiry, and how large the penalty is for vouching for a bad key.
2. **Vouch rules:** who may grant `trusted` (verifiers only, or trusted keys too).
3. **Pseudonym salt:** rotation schedule, and what rotating does to distinct-contributor counts.
4. **Time zones:** `sold_out_time.local_time` is assumed to be local time at the place (IST for Kerala). Confirm and document for other regions.
5. **`meal_times` derivation:** whether the system should derive coarse meal times from exact schedules.
6. **Merge thresholds:** the distance and name-similarity values that trigger a duplicate suggestion.
7. **`ready_to_map` threshold:** how many independent confirmations count as "existence confirmed".
8. **Property governance:** how long a `proposed` property can sit before it is promoted or dropped.
