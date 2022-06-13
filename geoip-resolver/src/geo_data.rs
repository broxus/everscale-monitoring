use std::io::Read;
use std::net::SocketAddrV4;
use std::path::Path;

use anyhow::{Context, Result};
use everscale_network::utils::FxHashMap;
use indicatif::ProgressBar;
use pomfrit::formatter::DisplayPrometheusExt;
use serde::{Deserialize, Serialize};

pub struct GeoDataImporter {
    db: GeoDb,
}

impl GeoDataImporter {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let mut options = rocksdb::Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        options.set_keep_log_file_num(1);

        let db = rocksdb::DB::open_cf(&options, path, CFS)
            .map(GeoDb)
            .context("Failed to open rocksdb in write mode")?;

        Ok(Self { db })
    }

    pub fn import_asn<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let count = count_lines(&path)?;
        let pb = ProgressBar::new(count);
        pb.println("Importing ASN");

        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .from_path(path)?;

        self.db.clear_cf(CF_ASN)?;
        let asn_cf = self.db.get_cf(CF_ASN)?;

        for item in reader.deserialize() {
            let AsnRecord {
                ip_from,
                mask,
                asn,
                name,
                ..
            } = item?;

            self.db.0.put_cf(
                asn_cf,
                ip_from.to_be_bytes(),
                bincode::serialize(&StoredAsnRecord { mask, asn, name }).expect("Shouldn't fail"),
            )?;

            pb.inc(1);
        }

        Ok(())
    }

    pub fn import_locations<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let count = count_lines(&path)?;
        let pb = ProgressBar::new(count);
        pb.println("Importing locations");

        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .from_path(path)?;

        self.db.clear_cf(CF_LOCATIONS)?;
        let location_cf = self.db.get_cf(CF_LOCATIONS)?;

        for item in reader.deserialize() {
            let LocationRecord {
                ip_from,
                country_code,
                country_name,
                region_name,
                city_name,
                latitude,
                longitude,
                ..
            } = item?;

            self.db.0.put_cf(
                location_cf,
                ip_from.to_be_bytes(),
                bincode::serialize(&StoredLocationRecord {
                    country_code,
                    country_name,
                    region_name,
                    city_name,
                    latitude,
                    longitude,
                })
                .expect("Shouldn't fail"),
            )?;

            pb.inc(1);
        }

        Ok(())
    }
}

pub struct GeoDataReader {
    db: GeoDb,
}

impl GeoDataReader {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = rocksdb::DB::open_cf_for_read_only(&Default::default(), path, CFS, false)
            .map(GeoDb)
            .context("Failed to open rocksdb in read mode")?;

        Ok(Self { db })
    }

    pub fn with_cfs<F>(&self, f: F) -> Result<()>
    where
        for<'a> F: FnOnce(Resolver<'a>) -> Result<()>,
    {
        let snapshot = self.db.0.snapshot();
        let location_iter = snapshot.raw_iterator_cf_opt(
            self.db.get_cf(CF_LOCATIONS)?,
            rocksdb::ReadOptions::default(),
        );
        let asn_iter =
            snapshot.raw_iterator_cf_opt(self.db.get_cf(CF_ASN)?, rocksdb::ReadOptions::default());

        f(Resolver {
            cache: LocationCache::default(),
            location_iter,
            asn_iter,
        })
    }
}

pub struct Resolver<'a> {
    cache: LocationCache,
    location_iter: rocksdb::DBRawIterator<'a>,
    asn_iter: rocksdb::DBRawIterator<'a>,
}

impl Resolver<'_> {
    pub fn find(&mut self, address: SocketAddrV4) -> Result<AddressInfo> {
        let ip = u32::from(*address.ip()).to_be_bytes();

        self.location_iter.seek_for_prev(&ip);
        let location: Option<StoredLocationRecord> = match self.location_iter.value() {
            Some(data) => Some(bincode::deserialize(data)?),
            None => None,
        };

        self.asn_iter.seek_for_prev(&ip);
        let other: Option<StoredAsnRecord> = match self.asn_iter.value() {
            Some(data) => Some(bincode::deserialize(data)?),
            None => None,
        };

        let adjusted_location = location.as_ref().map(|location| {
            self.cache
                .adjust((location.latitude, location.longitude), 0.0001)
        });

        Ok(AddressInfo {
            address,
            location,
            other,
            adjusted_location,
        })
    }
}

struct GeoDb(rocksdb::DB);

impl GeoDb {
    fn get_cf(&'_ self, name: &'static str) -> Result<&'_ rocksdb::ColumnFamily> {
        self.0
            .cf_handle(name)
            .with_context(|| format!("{} column family not found", name))
    }

    fn clear_cf(&mut self, name: &'static str) -> Result<()> {
        self.0.drop_cf(name)?;
        self.0.create_cf(name, &Default::default())?;
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AddressInfo {
    pub address: SocketAddrV4,
    pub location: Option<StoredLocationRecord>,
    pub other: Option<StoredAsnRecord>,
    pub adjusted_location: Option<(f64, f64)>,
}

impl std::fmt::Display for AddressInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let mut m = f
            .begin_metric("adnl_peer")
            .label("ip", self.address.ip())
            .label("port", self.address.port());

        if let Some(location) = &self.location {
            m = m
                .label("latitude", location.latitude)
                .label("longitude", location.longitude)
                .label_opt("country_code", &location.country_code)
                .label_opt("country_name", &location.country_name)
                .label_opt("region_name", &location.region_name)
                .label_opt("city_name", &location.city_name);
        }

        if let Some((latitude, longitude)) = &self.adjusted_location {
            m = m
                .label("adjusted_latitude", latitude)
                .label("adjusted_longitude", longitude);
        }

        if let Some(other) = &self.other {
            m = m
                .label_opt("asn", &other.asn)
                .label_opt("asn_name", &other.name);
        }

        m.value(1)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredLocationRecord {
    pub country_code: Option<String>,
    pub country_name: Option<String>,
    pub region_name: Option<String>,
    pub city_name: Option<String>,
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Deserialize)]
struct LocationRecord {
    ip_from: u32,
    _ip_to: u32,
    #[serde(deserialize_with = "deserialize_optional")]
    country_code: Option<String>,
    #[serde(deserialize_with = "deserialize_optional")]
    country_name: Option<String>,
    #[serde(deserialize_with = "deserialize_optional")]
    region_name: Option<String>,
    #[serde(deserialize_with = "deserialize_optional")]
    city_name: Option<String>,
    latitude: f64,
    longitude: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredAsnRecord {
    pub mask: Option<String>,
    pub asn: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AsnRecord {
    ip_from: u32,
    _ip_to: u32,
    #[serde(deserialize_with = "deserialize_optional")]
    mask: Option<String>,
    #[serde(deserialize_with = "deserialize_optional")]
    asn: Option<String>,
    #[serde(deserialize_with = "deserialize_optional")]
    name: Option<String>,
}

#[derive(Default)]
struct LocationCache(FxHashMap<(u64, u64), usize>);

impl LocationCache {
    fn adjust(&mut self, (latitude, longitude): (f64, f64), step: f64) -> (f64, f64) {
        let counter = self
            .0
            .entry((convert_f64(&latitude), convert_f64(&longitude)))
            .or_default();
        *counter += 1;

        let (offset_lat, offset_lng) = spiral(*counter as i64);

        (
            latitude + (offset_lat as f64) * step,
            longitude + (offset_lng as f64) * step,
        )
    }
}

fn spiral(n: i64) -> (i64, i64) {
    let k = (((n as f64).sqrt() - 1.0) / 2.0).ceil() as i64;
    let mut t = 2 * k + 1;
    let mut m = t * t;
    t -= 1;

    if n >= m - t {
        return (k - (m - n), -k);
    } else {
        m -= t;
    }

    if n >= m - t {
        return (-k, -k + (m - n));
    } else {
        m -= t;
    }

    if n >= m - t {
        (-k + (m - n), k)
    } else {
        (k, k - (m - n - t))
    }
}

fn deserialize_optional<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    String::deserialize(deserializer).map(|field| if field != "-" { Some(field) } else { None })
}

fn count_lines<P: AsRef<Path>>(path: P) -> Result<u64> {
    let file = std::fs::OpenOptions::new().read(true).open(path)?;

    let mut result = 0;
    let mut buffer = [0u8; 128];

    let mut reader = std::io::BufReader::new(file);
    loop {
        let bytes = reader.read(&mut buffer)?;
        if bytes == 0 {
            return Ok(result);
        }

        result += buffer[..bytes]
            .iter()
            .filter(|&symbol| *symbol == b'\n')
            .count() as u64;
    }
}

fn convert_f64(f: &f64) -> u64 {
    const SIGN_MASK: u64 = 0x8000000000000000u64;
    const EXP_MASK: u64 = 0x7ff0000000000000u64;
    const MAN_MASK: u64 = 0x000fffffffffffffu64;

    const CANONICAL_NAN_BITS: u64 = 0x7ff8000000000000u64;
    const CANONICAL_ZERO_BITS: u64 = 0x0u64;

    if f.is_nan() {
        return CANONICAL_NAN_BITS;
    }

    let bits = f.to_bits();

    let sign = if bits >> 63 == 0 { 1i8 } else { -1 };
    let mut exp = ((bits >> 52) & 0x7ff) as i16;
    let man = if exp == 0 {
        (bits & 0xfffffffffffff) << 1
    } else {
        (bits & 0xfffffffffffff) | 0x10000000000000
    };

    if man == 0 {
        return CANONICAL_ZERO_BITS;
    }

    // Exponent bias + mantissa shift
    exp -= 1023 + 52;

    let exp_u64 = exp as u64;
    let sign_u64 = if sign > 0 { 1u64 } else { 0u64 };
    (man & MAN_MASK) | ((exp_u64 << 52) & EXP_MASK) | ((sign_u64 << 63) & SIGN_MASK)
}

const CFS: &[&str] = &[CF_ASN, CF_LOCATIONS];

const CF_ASN: &str = "asn";
const CF_LOCATIONS: &str = "locations";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spiral() {
        for i in 0..100 {
            let (x, y) = spiral(i);
            println!("n={}: {} _ {}", i, x, y);
        }
    }
}
