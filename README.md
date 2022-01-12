## Everscale monitoring node

### Runtime requirements

- CPU: 4 cores, 2 GHz
- RAM: 8 GB
- Storage: 100 GB fast SSD
- Network: 100 MBit/s

### How to run

```bash
./scripts/setup.sh
```

After 20~30 minutes:

```bash
curl http://127.0.0.1:10000/metrics
```

### Config example
```yaml
---
node_settings:
  db_path: "/var/db/everscale-monitoring"
metrics_settings:
  listen_address: "0.0.0.0:10000"
  metrics_path: "/metrics"
  collection_interval_sec: 10
logger_settings:
  appenders:
    stdout:
      kind: console
      encoder:
        pattern: "{h({l})} {M} = {m} {n}"
  root:
    level: info
    appenders:
      - stdout
  loggers:
    tiny_adnl:
      level: error
      appenders:
        - stdout
      additive: false
```

### GeoIP

#### Prepare DB
1. Download [`IP2LOCATION-LITE-ASN.CSV`](https://lite.ip2location.com/database-asn) and [`IP2LOCATION-LITE-DB11.CSV`](https://lite.ip2location.com/database/db11-ip-country-region-city-latitude-longitude-zipcode-timezone)

2. Import databases:
   ```bash
   geoip-resolver import \
     --db /var/db/geodb \
     --asn IP2LOCATION-LITE-ASN.CSV \
     --locations IP2LOCATION-LITE-DB11.CSV
   ```
  
3. Search nodes:
   ```bash
   geoip-resolver resolve nodes.txt \
     --db /var/db/geodb \
     -g /etc/everscale-monitoring/ton-global.config.json
   ```
