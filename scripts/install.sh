#!/usr/bin/env bash
# Akasha installation script (Linux / macOS)
# Release archives include playwright-runner/ beside the binaries; Node.js is only needed for the managed browser feature.
# Usage: ./install.sh [--dir DIR] [--no-auto-start] [--download-embedded] [--skip-embedded-download]

set -e

INSTALL_DIR=""
NO_AUTO_START=false
DOWNLOAD_EMBEDDED=false
SKIP_EMBEDDED_DOWNLOAD=false
while [[ $# -gt 0 ]]; do
    case $1 in
        --dir) INSTALL_DIR="$2"; shift 2 ;;
        --no-auto-start) NO_AUTO_START=true; shift ;;
        --download-embedded) DOWNLOAD_EMBEDDED=true; shift ;;
        --skip-embedded-download) SKIP_EMBEDDED_DOWNLOAD=true; shift ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# Script dir: when run from extracted zip, we may be in . or in scripts/
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PARENT_DIR="$(dirname "$SCRIPT_DIR")"
if [[ -f "$SCRIPT_DIR/akasha" ]]; then
    BIN_DIR="$SCRIPT_DIR"
elif [[ -f "$PARENT_DIR/akasha" ]]; then
    BIN_DIR="$PARENT_DIR"
else
    BIN_DIR="$SCRIPT_DIR"
fi

for exe in akasha akasha-daemon akasha-tui; do
    if [[ ! -f "$BIN_DIR/$exe" ]]; then
        echo "Binary not found: $BIN_DIR/$exe. Run this script from the folder where you extracted the release." >&2
        exit 1
    fi
done

if [[ -z "$INSTALL_DIR" ]]; then
    if [[ -w /usr/local/bin ]]; then
        INSTALL_DIR="/usr/local/bin"
    else
        INSTALL_DIR="$HOME/.local/bin"
        mkdir -p "$INSTALL_DIR"
    fi
fi

mkdir -p "$INSTALL_DIR"
cp "$BIN_DIR/akasha" "$BIN_DIR/akasha-daemon" "$BIN_DIR/akasha-tui" "$INSTALL_DIR/"
chmod +x "$INSTALL_DIR/akasha" "$INSTALL_DIR/akasha-daemon" "$INSTALL_DIR/akasha-tui"
for sub in docs playwright-runner spec; do
    if [[ -d "$BIN_DIR/$sub" ]]; then
        cp -a "$BIN_DIR/$sub" "$INSTALL_DIR/"
    fi
done
echo "Binaries installed to $INSTALL_DIR"

# Initial setup (idempotent)
export PATH="$INSTALL_DIR:$PATH"
akasha init --defaults || true

echo ""
echo "Build CPU (Candle) par defaut sur ce zip ; premier appel potentiellement lent (1-3 min)."
if [[ "$DOWNLOAD_EMBEDDED" == true ]]; then
    echo "Telechargement du modele embarque..."
    akasha config models embedded-download || echo "embedded-download a echoue — relancez manuellement ou via l'assistant UI."
elif [[ "$SKIP_EMBEDDED_DOWNLOAD" != true ]]; then
    if akasha doctor --json 2>/dev/null | grep -q '"action".*"embedded-download"'; then
        read -r -p "Telecharger le modele GGUF embarque (~1 Go) ? [Y/n] " r
        if [[ "$r" != "n" && "$r" != "N" ]]; then
            akasha config models embedded-download || true
        fi
    fi
fi

# Ensure ~/.local/bin in PATH for current user
if [[ "$INSTALL_DIR" == "$HOME/.local/bin" ]]; then
    if ! echo ":$PATH:" | grep -q ":$HOME/.local/bin:"; then
        echo "Add to your shell profile: export PATH=\"\$HOME/.local/bin:\$PATH\""
    fi
fi

# Daemon at login
if [[ "$NO_AUTO_START" == true ]]; then
    echo "Skipped auto-start. To start the daemon: akasha start"
    echo "Starting daemon once..."
    nohup "$INSTALL_DIR/akasha" start >/dev/null 2>&1 &
    echo "Daemon started."
    echo "Installation complete."
    exit 0
fi

if [[ "$(uname -s)" == "Linux" ]]; then
    # systemd user service
    mkdir -p "$HOME/.config/systemd/user"
    cat > "$HOME/.config/systemd/user/akasha-daemon.service" << 'EOF'
[Unit]
Description=Akasha daemon
After=network.target

[Service]
Type=simple
ExecStart=/usr/bin/env akasha start --foreground
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF
    # Use full path if we know it
    if [[ -n "$INSTALL_DIR" ]]; then
        sed -i "s|ExecStart=.*|ExecStart=$INSTALL_DIR/akasha start --foreground|" "$HOME/.config/systemd/user/akasha-daemon.service"
    fi
    systemctl --user daemon-reload
    systemctl --user enable akasha-daemon.service
    echo "systemd user service enabled."
    echo "Starting daemon once..."
    systemctl --user start akasha-daemon.service 2>/dev/null || true
elif [[ "$(uname -s)" == "Darwin" ]]; then
    # launchd user agent
    PLIST="$HOME/Library/LaunchAgents/app.akasha.daemon.plist"
    AKASHA_PATH="$INSTALL_DIR/akasha"
    cat > "$PLIST" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>app.akasha.daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>$AKASHA_PATH</string>
    <string>start</string>
    <string>--foreground</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
</dict>
</plist>
EOF
    launchctl load "$PLIST"
    echo "launchd agent loaded. Daemon will start at login."
    echo "Starting daemon once..."
    launchctl start app.akasha.daemon 2>/dev/null || nohup "$AKASHA_PATH" start >/dev/null 2>&1 &
else
    echo "Auto-start not configured for this OS. Run 'akasha start' to start the daemon."
    echo "Starting daemon once..."
    nohup "$INSTALL_DIR/akasha" start >/dev/null 2>&1 &
fi
echo "Installation complete."
