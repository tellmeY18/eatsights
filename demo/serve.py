#!/usr/bin/env python3
"""Static server for the temporary demo page (not part of the Eatsights product).

Serves only:  GET /            -> demo/index.html
              GET /demo/<path> -> demo/<path>
Everything else is 404, so it can never shadow an API route. Bind to loopback; a
Cloudflare tunnel routes https://develop.letsimpl.fyi/ and /demo/* here and the rest of
the host to the API.

    python3 demo/serve.py [port]     # default 8095
"""
import http.server
import re
import mimetypes
import os
import sys

ROOT = os.path.dirname(os.path.abspath(__file__))
mimetypes.add_type("text/javascript", ".js")
mimetypes.add_type("text/javascript", ".mjs")
mimetypes.add_type("font/woff2", ".woff2")

ASSET = re.compile(rb"(/demo/[\w./-]+\.(?:js|css))(?![\w?])")


def version():
    """Changes whenever any demo file changes: busts CDN and browser caches."""
    newest = 0
    for base, _, files in os.walk(ROOT):
        for f in files:
            newest = max(newest, int(os.path.getmtime(os.path.join(base, f))))
    return str(newest).encode()


class Handler(http.server.BaseHTTPRequestHandler):
    server_version = "eatsights-demo"
    sys_version = ""

    def resolve(self):
        path = self.path.split("?", 1)[0].split("#", 1)[0]
        if path in ("/", "/index.html"):
            full = os.path.join(ROOT, "index.html")
            return full if os.path.isfile(full) else None
        if not path.startswith("/demo/"):
            return None
        rel = path[len("/demo/"):]
        full = os.path.realpath(os.path.join(ROOT, rel))
        if not full.startswith(ROOT + os.sep) or "/." in path:
            return None
        return full if os.path.isfile(full) else None

    def send(self, head_only):
        full = self.resolve()
        if full is None:
            body = b"not found\n"
            self.send_response(404)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if not head_only:
                self.wfile.write(body)
            return
        with open(full, "rb") as f:
            body = f.read()
        ctype = mimetypes.guess_type(full)[0] or "application/octet-stream"
        # Stamp asset references with a version so a CDN or browser that ignores no-cache
        # (e.g. a zone-wide Browser Cache TTL) still loads the current files.
        if ctype in ("text/html", "text/javascript"):
            v = version()
            body = ASSET.sub(lambda m: m.group(1) + b"?v=" + v, body)
        if ctype.startswith("text/") or ctype == "application/json":
            ctype += "; charset=utf-8"
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-cache")
        self.send_header("X-Content-Type-Options", "nosniff")
        # Origin only, cross-site: the OSM tile usage policy needs a valid Referer.
        self.send_header("Referrer-Policy", "strict-origin-when-cross-origin")
        # Same-origin API, plus read-only lookups made in the browser at view time:
        # OSM map tiles, Wikidata, Wikipedia summaries and Commons images. Nothing from these
        # sources is stored in Eatsights.
        self.send_header(
            "Content-Security-Policy",
            "default-src 'self'; "
            "img-src 'self' data: https://tile.openstreetmap.org https://upload.wikimedia.org https://thumb.wikimedia.org; "
            "connect-src 'self' https://www.wikidata.org https://en.wikipedia.org https://ml.wikipedia.org https://commons.wikimedia.org; "
            "style-src 'self'; font-src 'self'; frame-ancestors 'none'; base-uri 'none'",
        )
        self.end_headers()
        if not head_only:
            self.wfile.write(body)

    def do_GET(self):
        self.send(False)

    def do_HEAD(self):
        self.send(True)

    def log_message(self, fmt, *args):
        sys.stderr.write("%s %s\n" % (self.address_string(), fmt % args))


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8095
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"demo on http://127.0.0.1:{port}/", flush=True)
    srv.serve_forever()
