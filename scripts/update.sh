#!/usr/bin/env bash
set -eE

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
REPO_DIR=$(cd "${SCRIPT_DIR}/../" && pwd -P)

function print_help() {
  echo 'Usage: update.sh [OPTIONS]'
  echo ''
  echo 'Options:'
  echo '  -h,--help         Print this help message and exit'
  echo '  -f,--force        Clear "/var/db/everscale-monitoring" on update'
  echo '  -s,--sync         Restart "timesyncd" service'
}

force="false"
restart_timesyncd="false"
while [[ $# -gt 0 ]]; do
  key="$1"
  case $key in
      -h|--help)
        print_help
        exit 0
      ;;
      -f|--force)
        force="true"
        shift # past argument
      ;;
      -s|--sync)
        restart_timesyncd="true"
        shift # past argument
      ;;
      *) # unknown option
        echo 'ERROR: Unknown option'
        echo ''
        print_help
        exit 1
      ;;
  esac
done

echo "INFO: stopping everscale-monitoring service"
sudo systemctl stop everscale-monitoring

echo "INFO: stopping geoip service"
sudo systemctl stop geoip

if [[ "$force" == "true" ]]; then
  echo "INFO: removing everscale-monitoring db"
  sudo rm -rf /var/db/everscale-monitoring
else
  echo 'INFO: skipping "/var/db/everscale-monitoring" deletion'
fi

echo 'INFO: running update for native installation'

echo 'INFO: building everscale-monitoring'
cd "$REPO_DIR"
RUSTFLAGS="-C target_cpu=native" cargo build --release
sudo cp "$REPO_DIR/target/release/everscale-monitoring" /usr/local/bin/everscale-monitoring

echo 'INFO: building geoip-resolver'
cd "$REPO_DIR"
RUSTFLAGS="-C target_cpu=native" cargo build --release -p geoip-resolver
sudo cp "$REPO_DIR/target/release/geoip-resolver" /usr/local/bin/geoip-resolver


sudo wget -O /etc/everscale-monitoring/ton-global.config.json \
  https://raw.githubusercontent.com/tonlabs/main.ton.dev/master/configs/ton-global.config.json

echo "INFO: preparing environment"
sudo mkdir -p /var/db/everscale-monitoring
sudo mkdir -p /var/db/geoip

if [[ "$restart_timesyncd" == "true" ]]; then
  echo 'INFO: restarting timesyncd'
  sudo systemctl restart systemd-timesyncd.service
fi

echo 'INFO: restarting everscale-monitoring service'
sudo systemctl restart everscale-monitoring

echo 'INFO: restarting geoip service'
sudo systemctl restart geoip

echo 'INFO: done'
echo ''
echo 'INFO: Systemd service: everscale-monitoring'
echo '      Systemd geoip service: geoip-service'
echo '      Keys and configs: /etc/everscale-monitoring'
echo '      Node DB and stuff: /var/db/everscale-monitoring'
echo '      Geoip resolver DB: /var/db/geoip'
