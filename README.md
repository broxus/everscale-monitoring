## Everscale monitoring node

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
