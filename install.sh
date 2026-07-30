#!/bin/sh
set -eu

REPO="superhq-ai/neko-computer"
INSTALL_DIR="$HOME/.local/bin"
PLATFORM=""

##### Platform checks

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Darwin)
        if [ "$ARCH" != "arm64" ]; then
            echo "Error: neko requires Apple Silicon (arm64) on macOS. Detected: $ARCH" >&2
            exit 1
        fi
        PLATFORM="darwin-aarch64"
        ;;
    Linux)
        case "$ARCH" in
            aarch64|arm64)
                PLATFORM="linux-aarch64"
                ;;
            *)
                echo "Error: neko Linux builds currently support ARM64 only. Detected: $ARCH" >&2
                exit 1
                ;;
        esac
        ;;
    *)
        echo "Error: neko only supports macOS and Linux. Detected: $OS" >&2
        exit 1
        ;;
esac

##### Fetch latest release tag

echo "Fetching latest release..."
TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')

if [ -z "$TAG" ]; then
    echo "Error: could not determine latest release." >&2
    exit 1
fi

VERSION="${TAG#v}"
echo "Latest version: $VERSION"

##### Download and extract

TARBALL="neko-v${VERSION}-${PLATFORM}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${TAG}/${TARBALL}"

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

echo "Downloading ${TARBALL}..."
curl -fsSL "$URL" -o "$TMPDIR/$TARBALL"

mkdir -p "$INSTALL_DIR"
tar -xzf "$TMPDIR/$TARBALL" -C "$INSTALL_DIR"
chmod +x "$INSTALL_DIR/neko"
if [ "$OS" = "Darwin" ]; then
    xattr -d com.apple.quarantine "$INSTALL_DIR/neko" 2>/dev/null || true
fi

echo ""
echo "Installed neko $VERSION to $INSTALL_DIR/neko"

##### PATH check

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        echo ""
        echo "Add $INSTALL_DIR to your PATH:"
        echo ""
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
        echo ""
        echo "Add the line above to your shell profile to make it permanent."
        ;;
esac

echo ""
echo "Note: neko run boots a microVM. It downloads a small base image on first run,"
echo "or reuses an existing shuru install (https://github.com/superhq-ai/shuru) if present."
