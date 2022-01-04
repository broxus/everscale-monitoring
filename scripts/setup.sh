#!/usr/bin/env bash
set -eE

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
REPO_DIR=$(cd "${SCRIPT_DIR}/../" && pwd -P)

function print_help() {
  echo 'Usage: setup.sh'
  echo ''
  echo 'Options:'
  echo '  -h,--help         Print this help message and exit'
}

while [[ $# -gt 0 ]]; do
  key="$1"
  case $key in
      -h|--help)
        print_help
        exit 0
      ;;
      *) # unknown option
        echo 'ERROR: Unknown option'
        echo ''
        print_help
        exit 1
      ;;
  esac
done

service_path="/etc/systemd/system/everscale-monitoring.service"
config_path="/etc/everscale-monitoring/config.yaml"

echo 'INFO: Running native installation'

echo 'INFO: installing and updating dependencies'
sudo apt update && sudo apt upgrade
sudo apt install build-essential llvm clang

echo 'INFO: installing Rust'
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

echo 'INFO: building everscale-monitoring'
cd "$REPO_DIR"
RUSTFLAGS="-C target_cpu=native" cargo build --release
sudo cp "$REPO_DIR/target/release/everscale-monitoring" /usr/local/bin/everscale-monitoring

echo 'INFO: creating systemd service'
if [[ -f "$service_path" ]]; then
  echo "WARN: $service_path already exists"
else
  sudo cp "$REPO_DIR/contrib/everscale-monitoring.native.service" "$service_path"
fi


echo "INFO: preparing environment"
sudo mkdir -p /etc/everscale-monitoring
sudo mkdir -p /var/db/everscale-monitoring
if [[ -f "$config_path" ]]; then
  echo "WARN: $config_path already exists"
else
  sudo cp -n "$REPO_DIR/contrib/config.yaml" "$config_path"
fi

sudo wget -O /etc/everscale-monitoring/ton-global.config.json \
  https://raw.githubusercontent.com/tonlabs/main.ton.dev/master/configs/ton-global.config.json

echo 'INFO: restarting timesyncd'
sudo systemctl restart systemd-timesyncd.service

echo 'INFO: done'
echo ''
echo 'INFO: Systemd service: everscale-monitoring'
echo '      Keys and configs: /etc/everscale-monitoring'
echo '      Node DB and stuff: /var/db/everscale-monitoring'
