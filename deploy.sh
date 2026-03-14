#!/usr/bin/env bash
set -euo pipefail

WARTABLE_USER="${WARTABLE_USER:-wartable}"
WARTABLE_GROUP="$(id -g "$WARTABLE_USER" 2>/dev/null || echo "$WARTABLE_USER")"
WARTABLE_DIR="/opt/wartable"
SERVICE_FILE="/etc/systemd/system/wartable.service"

# --- First-run setup ---

setup_user() {
    if id "$WARTABLE_USER" &>/dev/null; then
        echo ":: user '$WARTABLE_USER' already exists"
        return
    fi

    echo ":: creating system user '$WARTABLE_USER'"
    sudo useradd --system --shell /usr/sbin/nologin \
        --home-dir "$WARTABLE_DIR" \
        --create-home \
        "$WARTABLE_USER"

    # GPU access
    for group in video render; do
        if getent group "$group" &>/dev/null; then
            sudo usermod -aG "$group" "$WARTABLE_USER"
            echo "   added to '$group' group"
        fi
    done
}

setup_dirs() {
    echo ":: setting up $WARTABLE_DIR"
    sudo mkdir -p "$WARTABLE_DIR"/{dashboard,jobs,logs}
    sudo chown -R "$WARTABLE_USER":"$WARTABLE_GROUP" "$WARTABLE_DIR"
}

install_service() {
    echo ":: installing systemd service (user=$WARTABLE_USER)"

    # Generate service file with correct user
    sudo tee "$SERVICE_FILE" > /dev/null << EOF
[Unit]
Description=wartable - GPU job scheduler
After=network.target

[Service]
Type=simple
User=$WARTABLE_USER
ExecStart=/usr/local/bin/wartable
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info
Environment=HOME=$WARTABLE_DIR

[Install]
WantedBy=multi-user.target
EOF

    sudo systemctl daemon-reload
}

# --- Build & deploy ---

echo ":: pulling latest"
git pull

echo ":: building release"
cargo build --release

# First-run detection
if ! id "$WARTABLE_USER" &>/dev/null; then
    echo ""
    echo "First-time setup detected."
    echo "This will create a '$WARTABLE_USER' system user to run jobs."
    echo "Set WARTABLE_USER=<name> to use a different user."
    echo ""
    read -p "Continue? [Y/n] " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Nn]$ ]]; then
        echo "Aborted."
        exit 1
    fi
    setup_user
fi

# Re-resolve group after potential user creation
WARTABLE_GROUP="$(id -g "$WARTABLE_USER" 2>/dev/null || echo "$WARTABLE_USER")"

setup_dirs

echo ":: installing binary"
sudo cp target/release/wartable /usr/local/bin/
sudo rsync -a --delete dashboard/ "$WARTABLE_DIR/dashboard/"
sudo chown -R "$WARTABLE_USER":"$WARTABLE_GROUP" "$WARTABLE_DIR"

install_service

echo ":: starting wartable"
sudo systemctl enable --now wartable
sudo systemctl restart wartable

echo ""
echo ":: status"
sudo systemctl status wartable --no-pager
echo ""
echo "done — http://$(hostname -I | awk '{print $1}'):9400"
echo "jobs run as: $WARTABLE_USER"
echo "working dir: $WARTABLE_DIR/jobs"
echo "logs:        $WARTABLE_DIR/logs"
echo "config:      $WARTABLE_DIR/.wartable/config.toml"
