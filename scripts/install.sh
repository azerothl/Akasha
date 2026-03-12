#!/usr/bin/env bash
# Akasha installation script (Linux / macOS)
# Usage: ./install.sh [--dir DIR] [--no-auto-start]
# --dir: install binaries to DIR (default: /usr/local/bin or ~/.local/bin if not writable)
# --no-auto-start: do not enable daemon at login

set -e

INSTALL_DIR=""
NO_AUTO_START=false
while [[ $# -gt 0 ]]; do
    case $1 in
        --dir) INSTALL_DIR="$2"; shift 2 ;;
        --no-auto-start) NO_AUTO_START=true; shift ;;
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
if [[ -f "$BIN_DIR/docs/user_guide.md" ]]; then
    mkdir -p "$INSTALL_DIR/../share/akasha/docs"
    cp "$BIN_DIR/docs/user_guide.md" "$INSTALL_DIR/../share/akasha/docs/" 2>/dev/null || true
fi
echo "Binaries installed to $INSTALL_DIR"

# Initial setup (idempotent)
export PATH="$INSTALL_DIR:$PATH"
akasha init --defaults || true

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
