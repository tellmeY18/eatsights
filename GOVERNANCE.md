# Governance (draft)

This dataset is public infrastructure. It belongs to no single app, including Chaikadi.

## Who decides

**BDFL: Vysakh Premkumar.** Until a sustained community forms, the BDFL makes final decisions on:

- accepting, changing or deprecating registry properties,
- schema versions and breaking changes,
- confirming place merges,
- granting `verifier` trust and holding the root key,
- who may write to the data repository.

Decisions are made in public, in issues and pull requests, with the reasoning written down.

## Commitments that cannot be changed by the BDFL alone

These exist so that other developers and contributors can rely on the dataset. Changing any of them requires the steering group described below.

1. **The data stays CC0 1.0.** Data already released under CC0 cannot be taken back.
2. **No paid listings.** No placement, ranking or trust level can be bought.
3. **No single app owns it.** Chaikadi writes through the same public API as every other app.
4. **Privacy.** No contributor location trails, no names, phone numbers or emails in the dataset. Raw contributor keys and the vouching graph are not published.
5. **Nothing is copied from sources with incompatible terms.** This includes OpenStreetMap, Google Maps, Zomato, Swiggy and similar. External data is linked by ID.
6. **Observations are never edited or deleted** to change history. Errors are corrected by newer observations. Legal or safety removals are logged publicly without the removed content.

## Fork right

Because the data is CC0, anyone can copy it and start a new project. This is a feature. It keeps the maintainers accountable.

## Moving beyond one person

When there are at least five regular contributors over three consecutive months (proposed threshold), the BDFL will convene a steering group that takes over the decisions above, with the BDFL as one member. The BDFL will also seek a neutral home for the project, for example a foundation or community organisation in the open-data ecosystem.

## Root key custody

- The root signing key is kept offline or on a hardware key, never on a server or in the repository.
- A backup root key is held by a second named person. The name is recorded in this file when chosen: `[TBD]`.
- If the root key is lost or compromised, the BDFL (or backup) publishes a signed notice from the backup root, and the trust chain is re-issued under a new root.

## Succession

If the BDFL becomes unable or unwilling to continue, the backup root key holder convenes the steering group within 60 days. Until then, the repository stays read-only except for security fixes.

## Changing this document

By pull request, announced publicly at least 14 days before merge, except for typo fixes.
