#!/usr/bin/env python3
"""DUMMY Overpass API for the develop demo. Never talks to OpenStreetMap.

The Eatsights OSM checker (EATSIGHTS_OSM_ENDPOINT) POSTs an Overpass query with
`around:<radius>,<lat>,<lng>`. This mock answers with obviously fake objects
(ids 900000000+) a few tens of metres from the pin, so the demo shows `candidate`,
`linked`, `unmapped` and `ready_to_map` without linking demo places to real map objects.

Pins listed in EMPTY get no results (-> unmapped). Everything else gets 1-3 dummies.

    python3 demo/mock-overpass.py [port]    # default 8097
"""
import http.server
import json
import math
import re
import sys
import urllib.parse
import zlib

# (lat, lng) of demo pins that should find nothing nearby.
EMPTY = [(9.6412, 76.4730), (10.0159, 76.3419)]
NAMES = ["Dummy Tea Stall", "Dummy Cafe", "Dummy Bakery", "Dummy Hotel", "Dummy Juice Shop"]
AMENITY = ["cafe", "cafe", "fast_food", "restaurant", "fast_food"]


def near(a, b, m=300):
    dy = (a[0] - b[0]) * 111_320
    dx = (a[1] - b[1]) * 111_320 * math.cos(math.radians(a[0]))
    return math.hypot(dx, dy) < m


def elements(lat, lng):
    if any(near((lat, lng), e) for e in EMPTY):
        return []
    seed = zlib.crc32(f"{lat:.4f},{lng:.4f}".encode())
    out = []
    for i in range((seed % 3) + 1):
        s = zlib.crc32(f"{seed}-{i}".encode())
        dist = 15 + (s % 70)                      # metres from the pin
        ang = (s >> 8) % 360
        dlat = dist * math.cos(math.radians(ang)) / 111_320
        dlng = dist * math.sin(math.radians(ang)) / (111_320 * math.cos(math.radians(lat)))
        k = (s >> 16) % len(NAMES)
        out.append({
            "type": "node",
            "id": 900_000_000 + (s % 99_999_999),
            "lat": round(lat + dlat, 7),
            "lon": round(lng + dlng, 7),
            "tags": {"name": NAMES[k], "amenity": AMENITY[k], "note": "dummy data, not OpenStreetMap"},
        })
    return out


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length") or 0)).decode()
        query = urllib.parse.parse_qs(body).get("data", [""])[0]
        m = re.search(r"around:(\d+),(-?[\d.]+),(-?[\d.]+)", query)
        if not m:
            self.send_response(400)
            self.end_headers()
            return
        lat, lng = float(m.group(2)), float(m.group(3))
        data = json.dumps({"version": 0.6, "generator": "eatsights dummy overpass",
                           "elements": elements(lat, lng)}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, fmt, *args):
        sys.stderr.write("mock-overpass %s\n" % (fmt % args))


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8097
    print(f"dummy overpass on http://127.0.0.1:{port}/api/interpreter", flush=True)
    http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
