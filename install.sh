#!/bin/bash
set -e

# REVENANT - Install Script
# Cognitive context restoration daemon
# https://github.com/derealt/revenant

REPO="derealt/revenant"
INSTALL_DIR="$HOME/.local/bin"
VERSION="0.2.0"

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

ARTIFACT="revenant-${VERSION}-${OS}-${ARCH}.tar.gz"
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/v${VERSION}/${ARTIFACT}"

echo "  │  Platform: ${OS}/${ARCH}"
echo "  │  Version:  ${VERSION}"
echo "  │"

# Create install directory
mkdir -p "$INSTALL_DIR"

# Download and extract
echo "  │  Downloading..."
if command -v curl &> /dev/null; then
  curl -fsSL "$DOWNLOAD_URL" -o "/tmp/${ARTIFACT}" 2>/dev/null || {
    echo "  │  ✘ Download failed. Check your access to the repo."
    echo "  │  URL: $DOWNLOAD_URL"
    echo "  └───"
    exit 1
  }
elif command -v wget &> /dev/null; then
  wget -q "$DOWNLOAD_URL" -O "/tmp/${ARTIFACT}" || {
    echo "  │  ✘ Download failed."
    echo "  └───"
    exit 1
  }
else
  echo "  │  ✘ Neither curl nor wget found."
  echo "  └───"
  exit 1
fi

echo "  │  Extracting..."
tar -xzf "/tmp/${ARTIFACT}" -C "$INSTALL_DIR"
rm -f "/tmp/${ARTIFACT}"
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
