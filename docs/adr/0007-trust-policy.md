# ADR-0007: Trust policy implementation (M4)

- Status: Proposed (implements the ADR-0006 defaults; maintainer to review)
- Date: 2026-11-14

## Context

ADR-0006 fixed the numbers (depth 4, `vendor_of` max 365 days, 2 strikes in 180 days, 30-day cooldown) and who may grant what. It mentions revocations "with reason `abuse`", but the signed `attestation` object has no `reason` field and schema changes need a proposal. This ADR records how M4 applies the policy without changing the schema.

## Decisions

1. **Levels are derived, not stored facts.** `private.keys.level` and `depth` are a cache of a pure function, `eatsights_core::trust::recompute`, over the private graph: the configured root (depth 0) plus every unrevoked, unexpired `vouch` and `rotate`. Every accepted attestation (and root upsert at startup) recomputes the whole graph inside the same transaction, under one advisory lock. The graph is small; this is simpler and safer than incremental bookkeeping.
   - A vouch counts while the issuer's *computed* level may grant it (issuer strictly above the granted level: member <- trusted/verifier/root, trusted <- verifier/root, verifier <- root) and issuer depth + 1 <= `EATSIGHTS_TRUST_MAX_DEPTH`.
   - A key takes the highest level offered, then the shortest depth. So a vouch never lowers a higher level, and revoking a vouch falls back to the remaining vouches; keys whose only chain ran through a lost key lose their standing too (cascade).
   - Issuers whose key is `revoked` stop counting. A `rotated` key keeps its standing for vouches it issued before rotating.
2. **Write-time checks** (rejected with problem+json): issuer active, `can_grant`, not in vouch cooldown, depth limit, subject registered and active, no self-attestation. Note: because levels strictly decrease along a chain, the natural maximum depth is 3 (root -> verifier -> trusted -> member); the default 4 is a guard, binding only when configured lower (e2e runs with 2).
3. **Revoke.** `vouch` and `vendor_of` can be revoked; `revoke` and `rotate` cannot (a rotation is undone by the new key rotating again, or by root revoking its vouches). Allowed: the original issuer, any verifier, root. The revoke's `subject_key_id` must equal the target's subject.
4. **Abuse strikes without a reason field.** A revocation of a *vouch* by root or a verifier who is **not** the vouch's issuer is treated as an abuse revocation and gives the vouch issuer one strike (`private.strikes`; root is exempt). An issuer revoking its own vouch is never a strike. This approximates ADR-0006's `reason: abuse`; a future schema proposal may add an explicit reason.
5. **Demotion.** When a key reaches `EATSIGHTS_TRUST_STRIKES` strikes within `..._STRIKE_WINDOW_DAYS` (counting only strikes after its last demotion), it gets `level_cap` = one level below its current level (floor `member`), `demoted_at` = now and `vouch_cooldown_until` = now + `..._VOUCH_COOLDOWN_DAYS`. The cap applies to vouches received up to `demoted_at`; a vouch issued after the demotion may lift it again (a deliberate re-grant). A demoted verifier disappears from `/v1/keys/published`.
6. **Rotate.** Signed by the old key; subject must be a registered, active, still `unverified` key. The old key becomes `rotated` with `successor_key_id`; the new key inherits level and depth through the rotate edge (no extra depth) and copies the strike cap and cooldown (no laundering). Root rotates by configuration (`EATSIGHTS_ROOT_PUBLIC_KEY`), not by attestation. Strikes for vouches issued by a rotated key go to its current successor.
7. **Vendor bindings follow rotation** by a `holder_key_id` column on `public.vendor_bindings`: the attestation and its id stay the same, rotation moves the holder (change feed op `transfer`), and revoking the original attestation still works. Listings show `holder_key_id` only when it differs from `subject_key_id`. Vendor keys are public by design (SCHEMA.md), so this exposes nothing new.
8. **`vendor_of` expiry.** Issuer must be verifier or root. Without `expires_at` the binding expires `issued_at` + 365 days (`EATSIGHTS_VENDOR_OF_MAX_DAYS`); a later or non-positive `expires_at` is rejected. The public document carries the effective `expires_at`. `GET /v1/attestations?type=vendor_of` lists only current bindings; `include_inactive=true` adds expired and revoked ones with `active: false`.
9. **Published keys** are exactly the `active` keys at level `verifier` or `root`, re-synced after each recompute (publish/unpublish change-feed entries).

## Accepted limitations

- Pseudonyms are per key, so a rotated contributor counts as a new distinct contributor at places they already observed. Rotation is rare; accepted.
- Bindings that expire do not bump the change feed, so a cached listing may show `active: true` until its max-age (60 s) passes or another change happens.
- Strike attribution from "non-issuer revoke" can be wrong when a verifier revokes a vouch for reasons other than abuse. Verifiers should ask the issuer to revoke instead; an explicit reason needs a schema proposal.
