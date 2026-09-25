#!/usr/bin/env bash
# Akasha unified setup (Linux / macOS)
# Run after extracting the "full" zip. Prompts for: install desktop app (Tauri), daemon at login.
# The release zip also includes playwright-runner/ next to the binaries (managed browser). Node.js + npm are required only if you use that feature; the daemon can auto-install deps on first use unless AKASHA_PLAYWRIGHT_AUTO_INSTALL=0.
# Usage: ./setup.sh [--dir DIR] [--install-ui] [--no-install-ui] [--auto-start] [--no-auto-start] [--download-embedded] [--skip-embedded-download]

set -e

INSTALL_DIR=""
DO_INSTALL_UI=""
DO_AUTO_START=""
DOWNLOAD_EMBEDDED=false
SKIP_EMBEDDED_DOWNLOAD=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --dir) INSTALL_DIR="$2"; shift 2 ;;
        --install-ui) DO_INSTALL_UI=true; shift ;;
        --no-install-ui) DO_INSTALL_UI=false; shift ;;
        --auto-start) DO_AUTO_START=true; shift ;;
        --no-auto-start) DO_AUTO_START=false; shift ;;
        --download-embedded) DOWNLOAD_EMBEDDED=true; shift ;;
        --skip-embedded-download) SKIP_EMBEDDED_DOWNLOAD=true; shift ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PARENT_DIR="$(dirname "$SCRIPT_DIR")"
if [[ -f "$PARENT_DIR/akasha" ]]; then
    ROOT_DIR="$PARENT_DIR"
elif [[ -f "$SCRIPT_DIR/akasha" ]]; then
    ROOT_DIR="$SCRIPT_DIR"
else
    ROOT_DIR="$PARENT_DIR"
fi

INSTALL_SCRIPT="$SCRIPT_DIR/install.sh"
[[ -f "$INSTALL_SCRIPT" ]] || INSTALL_SCRIPT="$ROOT_DIR/scripts/install.sh"
if [[ ! -f "$INSTALL_SCRIPT" ]]; then
    echo "install.sh not found. Run this script from the extracted Akasha full package." >&2
    exit 1
fi

echo "=== Akasha installation ==="
echo "This will install: CLI, daemon, TUI. Optionally: desktop app (web interface) and daemon at login."
echo ""

# Detect UI bundle (deb, AppImage, dmg)
UI_BUNDLE=$(find "$ROOT_DIR" -maxdepth 4 \( -name "*.deb" -o -name "*.AppImage" -o -name "*.dmg" \) 2>/dev/null | head -1)

# Prompt Install UI
if [[ -z "$DO_INSTALL_UI" ]]; then
    if [[ -n "$UI_BUNDLE" ]]; then
        read -r -p "Install desktop app (web interface)? [Y/n] " r
        DO_INSTALL_UI=$([[ "$r" =~ ^[nN] ]] && echo false || echo true)
    else
        DO_INSTALL_UI=false
    fi
fi

# Prompt Auto Start
if [[ -z "$DO_AUTO_START" ]]; then
    read -r -p "Start Akasha daemon at every login? [Y/n] " r
    DO_AUTO_START=$([[ "$r" =~ ^[nN] ]] && echo false || echo true)
fi

# Build install.sh args
INSTALL_ARGS=()
[[ -n "$INSTALL_DIR" ]] && INSTALL_ARGS+=(--dir "$INSTALL_DIR")
[[ "$DO_AUTO_START" == false ]] && INSTALL_ARGS+=(--no-auto-start)
[[ "$DOWNLOAD_EMBEDDED" == true ]] && INSTALL_ARGS+=(--download-embedded)
[[ "$SKIP_EMBEDDED_DOWNLOAD" == true ]] && INSTALL_ARGS+=(--skip-embedded-download)

# 1) Install CLI + daemon + TUI
"$INSTALL_SCRIPT" "${INSTALL_ARGS[@]}"

# 2) Optional: install Tauri desktop app
if [[ "$DO_INSTALL_UI" == true && -n "$UI_BUNDLE" ]]; then
    case "$(uname -s)" in
        Linux)
            if [[ "$UI_BUNDLE" == *.deb ]]; then
                echo "Installing desktop app (.deb)..."
                sudo dpkg -i "$UI_BUNDLE" || true
            elif [[ "$UI_BUNDLE" == *.AppImage ]]; then
                echo "Desktop app (AppImage): $UI_BUNDLE"
                chmod +x "$UI_BUNDLE"
                # Optional: copy to a standard location or leave in place
                echo "Run it with: $UI_BUNDLE"
            fi
            ;;
        Darwin)
            if [[ "$UI_BUNDLE" == *.dmg ]]; then
                echo "Opening .dmg — drag Akasha.app to Applications if desired."
                open "$UI_BUNDLE"
            fi
            ;;
        *) echo "Desktop app: $UI_BUNDLE (install manually if needed)" ;;
    esac
fi

# (Daemon was already started once by install.sh.)

echo ""
echo "Setup complete."
echo "  CLI: akasha (ensure install dir is in PATH)"
echo "  Daemon: already started; will restart at login if you chose auto-start."
AKASHA_CMD=""
for d in /usr/local/bin "$HOME/.local/bin"; do
    [[ -x "$d/akasha" ]] && AKASHA_CMD="$d/akasha" && break
done
read -r -p "Lancer l'assistant de configuration maintenant ? [Y/n] " r
if [[ "$r" != "n" && "$r" != "N" && -n "$AKASHA_CMD" ]]; then
    "$AKASHA_CMD" init || true
    echo "You can also run: akasha tui   (terminal UI)"
fi
