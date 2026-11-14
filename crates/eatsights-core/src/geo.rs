//! Geohash encode/decode and great-circle distance (no I/O, no PostGIS).

const BASE32: &[u8; 32] = b"0123456789bcdefghjkmnpqrstuvwxyz";

/// Bounding box of a geohash cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    pub lat_min: f64,
    pub lat_max: f64,
    pub lng_min: f64,
    pub lng_max: f64,
}

impl BBox {
    pub fn center(&self) -> (f64, f64) {
        (
            (self.lat_min + self.lat_max) / 2.0,
            (self.lng_min + self.lng_max) / 2.0,
        )
    }
}

/// Encode a point as a geohash of `precision` characters (1..=12).
pub fn encode(lat: f64, lng: f64, precision: usize) -> String {
    let (mut lat_lo, mut lat_hi) = (-90.0f64, 90.0f64);
    let (mut lng_lo, mut lng_hi) = (-180.0f64, 180.0f64);
    let mut out = String::with_capacity(precision);
    let mut even = true;
    let mut bit = 0;
    let mut ch = 0usize;
    while out.len() < precision.clamp(1, 12) {
        if even {
            let mid = (lng_lo + lng_hi) / 2.0;
            if lng >= mid {
                ch |= 1 << (4 - bit);
                lng_lo = mid;
            } else {
                lng_hi = mid;
            }
        } else {
            let mid = (lat_lo + lat_hi) / 2.0;
            if lat >= mid {
                ch |= 1 << (4 - bit);
                lat_lo = mid;
            } else {
                lat_hi = mid;
            }
        }
        even = !even;
        if bit < 4 {
            bit += 1;
        } else {
            out.push(char::from(BASE32[ch]));
            bit = 0;
            ch = 0;
        }
    }
    out
}

/// True for a non-empty geohash of at most 12 valid characters.
pub fn is_valid(cell: &str) -> bool {
    !cell.is_empty() && cell.len() <= 12 && cell.bytes().all(|c| BASE32.contains(&c))
}

/// Bounding box of a cell; `None` when the cell is not a valid geohash.
pub fn decode_bbox(cell: &str) -> Option<BBox> {
    if !is_valid(cell) {
        return None;
    }
    let (mut lat_lo, mut lat_hi) = (-90.0f64, 90.0f64);
    let (mut lng_lo, mut lng_hi) = (-180.0f64, 180.0f64);
    let mut even = true;
    for c in cell.bytes() {
        let v = BASE32.iter().position(|b| *b == c)?;
        for i in (0..5).rev() {
            let on = (v >> i) & 1 == 1;
            if even {
                let mid = (lng_lo + lng_hi) / 2.0;
                if on {
                    lng_lo = mid;
                } else {
                    lng_hi = mid;
                }
            } else {
                let mid = (lat_lo + lat_hi) / 2.0;
                if on {
                    lat_lo = mid;
                } else {
                    lat_hi = mid;
                }
            }
            even = !even;
        }
    }
    Some(BBox {
        lat_min: lat_lo,
        lat_max: lat_hi,
        lng_min: lng_lo,
        lng_max: lng_hi,
    })
}

/// The cell containing the point plus its 8 neighbours at `precision` (deduplicated, sorted).
pub fn cells_3x3(lat: f64, lng: f64, precision: usize) -> Vec<String> {
    let center = encode(lat, lng, precision);
    let Some(b) = decode_bbox(&center) else {
        return vec![center];
    };
    let (h, w) = (b.lat_max - b.lat_min, b.lng_max - b.lng_min);
    let (clat, clng) = b.center();
    let mut out = Vec::with_capacity(9);
    for dy in [-1.0, 0.0, 1.0] {
        for dx in [-1.0, 0.0, 1.0] {
            let la = (clat + dy * h).clamp(-89.999_999, 89.999_999);
            let mut lo = clng + dx * w;
            if lo >= 180.0 {
                lo -= 360.0;
            } else if lo < -180.0 {
                lo += 360.0;
            }
            out.push(encode(la, lo, precision));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Haversine distance in metres.
pub fn distance_m(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    let r = 6_371_000.0f64;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = (lat2 - lat1).to_radians();
    let dl = (lng2 - lng1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * r * a.sqrt().asin()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn known_vector() {
        assert_eq!(encode(57.64911, 10.40744, 11), "u4pruydqqvj");
        let b = decode_bbox("u4pruydqqvj").unwrap();
        assert!(b.lat_min <= 57.64911 && 57.64911 <= b.lat_max);
        assert!(b.lng_min <= 10.40744 && 10.40744 <= b.lng_max);
        assert!(decode_bbox("abc").is_none());
    }

    #[test]
    fn distance() {
        let d = distance_m(9.9312, 76.2673, 9.9816, 76.2999);
        assert!((d - 6600.0).abs() < 300.0, "{d}");
    }
}
