#!/usr/bin/env bash
set -euo pipefail

echo ":: pulling latest"
git pull

echo ":: building release"
cargo build --release

echo ":: installing"
sudo cp target/release/wartable /usr/local/bin/
sudo mkdir -p /opt/wartable/{dashboard,jobs,logs}
sudo cp -r dashboard/ /opt/wartable/dashboard/
sudo cp wartable.service /etc/systemd/system/
sudo systemctl daemon-reload

echo ":: restarting"
sudo systemctl enable --now wartable
sudo systemctl restart wartable

echo ":: status"
sudo systemctl status wartable --no-pager
echo ""
echo "done — http://$(hostname -I | awk '{print $1}'):9400"
