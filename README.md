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
  # UDP port, used for ADNL node. Default: 30303
  adnl_port: 30000

  # Root directory for the node DB. Default: "./db"
  db_path: "/var/db/everscale-monitoring"

  # Manual rocksdb memory options (will be computed from the
  # available memory otherwise).
  # db_options:
  #   rocksdb_lru_capacity: "512 MB"
  #   cells_cache_size: "4 GB"

  # Everscale specific network settings
  adnl_options:
    use_loopback_for_neighbours: true
    force_use_priority_channels: true
  rldp_options:
    force_compression: true
  overlay_shard_options:
    force_compression: true

metrics_settings:
  # Listen address of metrics. Used by the client to gather prometheus metrics.
  # Default: "127.0.0.1:10000"
  listen_address: "0.0.0.0:10000"
  # URL path to the metrics. Default: "/"
  # Example: `curl http://127.0.0.1:10000/metrics`
  metrics_path: "/metrics"
  # Metrics update interval in seconds. Default: 10
  collection_interval_sec: 10
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
