# Demo page (temporary, develop only)

A throwaway page at `https://develop.letsimpl.fyi/` that shows developers what the
Eatsights API does: browse places and menus, the property registry, make an Ed25519 key in
the browser, sign writes (places, prices), watch observations become facts, see what is
published, and follow the change feed and exports.

**Not part of the product.** Eatsights builds no UI (AGENTS.md section 1). This folder is
not compiled into or served by the `eatsights` binary, is excluded from the container
image (`.dockerignore`), and can be deleted at any time.

## How it is served

- `demo/serve.py` (Python stdlib) serves `GET /` → `index.html` and `GET /demo/*` → this
  folder, nothing else, on `127.0.0.1:8095`, with a strict Content-Security-Policy
  (same-origin only, no inline scripts or styles).
- The Cloudflare tunnel for `develop.letsimpl.fyi` routes `^/(demo/.*)?$` to it and every
  other path to the API on `127.0.0.1:8080`, so the page calls the API same-origin.

```sh
python3 demo/serve.py 8095
```

For local work against another API, open `index.html?api=https://develop.letsimpl.fyi`
from a server without the CSP (e.g. `python3 -m http.server --directory .` at the repo
root, then `/demo/index.html?api=...`); `serve.py`'s CSP blocks cross-origin calls.

## Files

`index.html`, `styles.css`, `app.js` (Preact + htm, vendored in `vendor/`, no build step),
`eatsights.js` (RFC 8785 canonical JSON, Ed25519 via WebCrypto, key ids, ULIDs, API calls),
`fonts/` (Atkinson Hyperlegible Mono, Courier Prime; SIL OFL 1.1).

## Tests

```sh
node --test demo/test/crypto.test.mjs   # canonical JSON + signatures equal the eatsights CLI byte for byte
node demo/test/live.mjs https://develop.letsimpl.fyi   # full write flow; writes public CC0 data
```

Data written through the page is real, public, permanent and CC0.
