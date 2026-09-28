#!/bin/bash
set -e

# REVENANT - Install Script
# Cognitive context restoration daemon
# https://github.com/derealt/revenant

REPO="derealt/revenant"
INSTALL_DIR="$HOME/.local/bin"
# Latest release unless REVENANT_VERSION pins one (e.g. REVENANT_VERSION=0.3.0)
VERSION="${REVENANT_VERSION:-}"

echo ""
echo "  ┌─── REVENANT ───"
echo "  │"
echo "  │  Cognitive context restoration."
echo "  │  Captures what you were doing. Restores it when you return."
echo "  │"

# Detect platform
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

if [ "$OS" != "darwin" ] && [ "$OS" != "linux" ]; then
  echo "  │  ✘ Unsupported platform: $OS (macOS and Linux are supported)"
  echo "  └───"
  exit 1
fi

if [ "$ARCH" = "x86_64" ]; then
  ARCH="x86_64"
elif [ "$ARCH" = "arm64" ] || [ "$ARCH" = "aarch64" ]; then
  ARCH="aarch64"
else
  echo "  │  ✘ Unsupported architecture: $ARCH"
  echo "  └───"
  exit 1
fi

if [ -z "$VERSION" ]; then
  # github.com/<repo>/releases/latest redirects to .../tag/vX.Y.Z
  LATEST_URL=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest" 2>/dev/null || true)
  VERSION="${LATEST_URL##*/v}"
  case "$VERSION" in
    [0-9]*) ;;
    *)
      echo "  │  ✘ Could not find the latest release. Set REVENANT_VERSION and retry."
      echo "  └───"
      exit 1
      ;;
  esac
fi

ARTIFACT="revenant-${VERSION}-${OS}-${ARCH}.tar.gz"
BASE_URL="https://github.com/${REPO}/releases/download/v${VERSION}"
DOWNLOAD_URL="${BASE_URL}/${ARTIFACT}"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

echo "  │  Platform: ${OS}/${ARCH}"
echo "  │  Version:  ${VERSION}"
echo "  │"

# Create install directory
mkdir -p "$INSTALL_DIR"

# Download and extract
echo "  │  Downloading..."
if command -v curl &> /dev/null; then
  curl -fsSL "$DOWNLOAD_URL" -o "$TMP_DIR/${ARTIFACT}" 2>/dev/null || {
    echo "  │  ✘ Download failed."
    echo "  │  URL: $DOWNLOAD_URL"
    echo "  └───"
    exit 1
  }
elif command -v wget &> /dev/null; then
  wget -q "$DOWNLOAD_URL" -O "$TMP_DIR/${ARTIFACT}" || {
    echo "  │  ✘ Download failed."
    echo "  └───"
    exit 1
  }
else
  echo "  │  ✘ Neither curl nor wget found."
  echo "  └───"
  exit 1
fi

# Verify the download against the release's published checksums
if curl -fsSL "${BASE_URL}/SHA256SUMS" -o "$TMP_DIR/SHA256SUMS" 2>/dev/null; then
  EXPECTED=$(grep " ${ARTIFACT}\$" "$TMP_DIR/SHA256SUMS" | awk '{print $1}')
  if command -v sha256sum &> /dev/null; then
    ACTUAL=$(sha256sum "$TMP_DIR/${ARTIFACT}" | awk '{print $1}')
  else
    ACTUAL=$(shasum -a 256 "$TMP_DIR/${ARTIFACT}" | awk '{print $1}')
  fi
  if [ -n "$EXPECTED" ] && [ "$EXPECTED" != "$ACTUAL" ]; then
    echo "  │  ✘ Checksum mismatch for ${ARTIFACT}. Not installing."
    echo "  └───"
    exit 1
  fi
  [ -n "$EXPECTED" ] && echo "  │  ✓ Checksum verified"
fi

echo "  │  Extracting..."
# Remove, never overwrite in place: on macOS rewriting a binary the kernel
# has already seen invalidates its cached signature and the daemon is killed
# An upgrade stops the running daemon first, so `rvn init` below starts the
# new binary instead of finding the old one "already loaded"
if [ -x "$INSTALL_DIR/rvn" ]; then
  "$INSTALL_DIR/rvn" off >/dev/null 2>&1 || true
fi
rm -f "$INSTALL_DIR/revenant" "$INSTALL_DIR/rvn"
tar -xzf "$TMP_DIR/${ARTIFACT}" -C "$INSTALL_DIR"
chmod +x "$INSTALL_DIR/revenant" "$INSTALL_DIR/rvn"

echo "  │  ✓ revenant → $INSTALL_DIR/revenant"
echo "  │  ✓ rvn      → $INSTALL_DIR/rvn"

# Add to PATH if needed
if ! echo "$PATH" | grep -q "$INSTALL_DIR"; then
  SHELL_RC=""
  if [ -f "$HOME/.zshrc" ]; then
    SHELL_RC="$HOME/.zshrc"
  elif [ -f "$HOME/.bashrc" ]; then
    SHELL_RC="$HOME/.bashrc"
  fi

  if [ -n "$SHELL_RC" ]; then
    if ! grep -q "$INSTALL_DIR" "$SHELL_RC" 2>/dev/null; then
      echo "export PATH=\"$INSTALL_DIR:\$PATH\"" >> "$SHELL_RC"
      echo "  │  ✓ Added $INSTALL_DIR to PATH in $SHELL_RC"
    fi
  fi
  export PATH="$INSTALL_DIR:$PATH"
fi

echo "  │"

# Run init
echo "  │  Initializing..."
"$INSTALL_DIR/rvn" init 2>&1 | sed 's/^/  │  /'

echo "  │"
echo "  │  ✓ REVENANT is installed and running."
echo "  │"
echo "  │  Open a new terminal to see the ghost."
echo "  │  Run 'rvn test' to inject a test card."
echo "  │  Run 'rvn status' to check the daemon."
echo "  │"
echo "  │  Optional:"
echo "  │    rvn setup llm     - Enable AI-powered context cards"
echo "  │    rvn setup browser  - Install Chrome extension"
echo "  │    rvn setup vscode   - Install VS Code extension"
echo "  └───"
echo ""
