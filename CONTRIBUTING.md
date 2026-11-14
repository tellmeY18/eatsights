# Contributing (draft)

Anyone can contribute. You don't need an account.

## Your contribution is CC0

By submitting data, you release it under [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/). Only submit what you saw, paid for, or know yourself.

## The one hard rule: never copy

Do **not** paste, scrape or retype data from:

- OpenStreetMap (its licence, ODbL, is incompatible with CC0),
- Google Maps, Zomato, Swiggy or any other app or website,
- another person's photos, menus or reviews.

Record only what you observed yourself: a price you paid, a menu board you photographed, a wait you experienced. If a fact exists in OSM or Wikidata, link its ID instead of copying its content.

## What you can contribute

| Contribution | How |
|---|---|
| A price, wait time, availability or quality rating | One tap in an app that writes to the public API |
| A new place | Drop a pin, name it, choose a category. The app should show nearby existing places first |
| A menu | Photograph the board. The photo is processed for text and discarded, never stored or published |
| A vendor's own menu and schedule | Through a vendor key issued by a verifier |
| A fix to a place or dish record | Pull request against the data repository |
| A new property | See below |

## Rules for what goes in

- **No free text.** Opinions are structured scores. There is no review or comment field, by design.
- **No personal data.** Don't include names, phone numbers or faces.
- **No images are stored by this project.** Dish images belong on Wikimedia Commons; add them there and link them from the Wikidata item.
- **One observation, one fact.** Don't bundle a price and a wait time into one record.

## Proposing a new property

1. Open a pull request adding an entry to `properties.json` with `status: proposed`. It must validate against `property.schema.json`.
2. Fill in: scope, a clear description, a value schema, a half-life, who may write it, and how observations combine.
3. Give two or three real examples.
4. The BDFL reviews it. Accepted proposals go live as `proposed`, marked as unstable in the output. After real observations arrive, the property is promoted to `accepted`.

## Improving OpenStreetMap

Places that exist but aren't on the map are listed in the ready-to-map export. If you know OSM, add them yourself through your own OSM account, following OSM's contribution guidelines, and the dataset will pick up the link later. Easier tools for this are app features, not part of the schema.

## Trust

New keys start unverified. Your observations count, but with less weight until other contributors corroborate them or a trusted person vouches for you. Vendors are verified in person by a verifier.

## Conduct

Be accurate and be kind. Deliberate false data, spam or attempts to track other contributors lead to key revocation.
