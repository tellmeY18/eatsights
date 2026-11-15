// Read-time enrichment: everything here is fetched in the browser when shown and kept only in
// memory. Eatsights stores the Wikidata Q-id and the place's own pin; nothing from Wikidata,
// Wikipedia, Commons or OpenStreetMap is copied into it.

const cache = new Map();
const once = (key, fn) => {
  if (!cache.has(key)) cache.set(key, fn().catch((e) => { cache.delete(key); throw e; }));
  return cache.get(key);
};
const json = async (url) => {
  const r = await fetch(url, { headers: { accept: 'application/json' }, credentials: 'omit' });
  if (!r.ok) throw new Error(`${new URL(url).host} ${r.status}`);
  return r.json();
};
const text = (html) => (html || '').replace(/<[^>]+>/g, '').replace(/\s+/g, ' ').trim();

// Wikidata item -> label, description, Wikipedia summary, Commons image with credit.
export function wikidata(qid) {
  return once('wd:' + qid, async () => {
    const data = await json(`https://www.wikidata.org/wiki/Special:EntityData/${encodeURIComponent(qid)}.json`);
    const e = data.entities?.[qid] || Object.values(data.entities || {})[0];
    if (!e || e.missing !== undefined) throw new Error(`${qid} not found on Wikidata`);
    const out = {
      qid: e.id,
      label: e.labels?.en?.value, labelMl: e.labels?.ml?.value,
      description: e.descriptions?.en?.value,
      wikidataUrl: `https://www.wikidata.org/wiki/${e.id}`,
    };
    // Wikipedia: English if there is an article, else Malayalam.
    for (const lang of ['en', 'ml']) {
      const title = e.sitelinks?.[`${lang}wiki`]?.title;
      if (!title) continue;
      try {
        const s = await json(`https://${lang}.wikipedia.org/api/rest_v1/page/summary/${encodeURIComponent(title)}`);
        out.wikipedia = { lang, title: s.title, extract: s.extract, url: s.content_urls?.desktop?.page };
        break;
      } catch { /* try the next language */ }
    }
    // Commons image (P18) with its author and licence.
    const file = e.claims?.P18?.[0]?.mainsnak?.datavalue?.value;
    if (file) {
      const q = await json(`https://commons.wikimedia.org/w/api.php?action=query&titles=${encodeURIComponent('File:' + file)}&prop=imageinfo&iiprop=url|extmetadata&iiurlwidth=480&format=json&origin=*`);
      const ii = Object.values(q.query?.pages || {})[0]?.imageinfo?.[0];
      if (ii) {
        out.image = {
          src: ii.thumburl || ii.url, page: ii.descriptionurl, file,
          artist: text(ii.extmetadata?.Artist?.value) || 'unknown author',
          licence: text(ii.extmetadata?.LicenseShortName?.value) || 'see Commons',
        };
      }
    }
    return out;
  });
}

// Web Mercator tile maths for a static map made of OSM tiles around a pin.
export const TILE = 256;
export function mapTiles(lat, lng, zoom, width, height) {
  const n = 2 ** zoom;
  const px = ((lng + 180) / 360) * n * TILE;
  const r = (lat * Math.PI) / 180;
  const py = ((1 - Math.log(Math.tan(r) + 1 / Math.cos(r)) / Math.PI) / 2) * n * TILE;
  const left = px - width / 2, top = py - height / 2;
  const tiles = [];
  for (let tx = Math.floor(left / TILE); tx <= Math.floor((left + width - 1) / TILE); tx++) {
    for (let ty = Math.floor(top / TILE); ty <= Math.floor((top + height - 1) / TILE); ty++) {
      if (ty < 0 || ty >= n) continue;
      const x = ((tx % n) + n) % n;
      tiles.push({ key: `${zoom}/${x}/${ty}`, src: `https://tile.openstreetmap.org/${zoom}/${x}/${ty}.png`, left: tx * TILE - left, top: ty * TILE - top });
    }
  }
  return tiles;
}
