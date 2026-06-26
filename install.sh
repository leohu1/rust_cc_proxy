#!/usr/bin/env bash
set -euo pipefail

# ── Config ──────────────────────────────────────────────────────────
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
CONFIG_DIR="${CONFIG_DIR:-$HOME/.config/rust_cc_proxy}"
REPO_URL="${REPO_URL:-https://github.com/leohu1/rust_cc_proxy.git}"
BRANCH="${BRANCH:-master}"
SKIP_RUST_CHECK="${SKIP_RUST_CHECK:-0}"
BUILD_RELEASE="${BUILD_RELEASE:-1}"

BOLD="\033[1m"; GREEN="\033[32m"; YELLOW="\033[33m"; RED="\033[31m"; CYAN="\033[36m"; RESET="\033[0m"

info()  { echo -e "${GREEN}[INFO]${RESET} $*"; }
warn()  { echo -e "${YELLOW}[WARN]${RESET} $*"; }
err()   { echo -e "${RED}[ERROR]${RESET} $*"; exit 1; }
step()  { echo -e "\n${CYAN}${BOLD}:: $*${RESET}\n"; }

# ── Banner ──────────────────────────────────────────────────────────
echo -e "${GREEN}${BOLD}"
echo "  ╭─────────────────────────────────────────────╮"
echo "  │  rust_cc_proxy — Claude Code Proxy Installer │"
echo "  ╰─────────────────────────────────────────────╯"
echo -e "${RESET}"

# ── OS detection ────────────────────────────────────────────────────
OS="$(uname -s)"
case "$OS" in
  Linux)   OS=linux ;;
  Darwin)  OS=macos ;;
  *)       err "Unsupported OS: $OS" ;;
esac
ARCH="$(uname -m)"
info "Detected: $OS / $ARCH"

# ── Check/install Rust ──────────────────────────────────────────────
if [ "$SKIP_RUST_CHECK" != "1" ]; then
  step "Checking Rust toolchain"
  if command -v cargo &>/dev/null; then
    RUST_VER="$(rustc --version | awk '{print $2}')"
    info "Rust $RUST_VER already installed"
  else
    info "Installing Rust via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
  fi
fi

# ── Clone / update repo ─────────────────────────────────────────────
REPO_DIR="$HOME/.rust_cc_proxy_repo"
if [ -d "$REPO_DIR/.git" ]; then
  step "Updating repository"
  cd "$REPO_DIR"
  git fetch origin "$BRANCH"
  git checkout "$BRANCH"
  git pull origin "$BRANCH"
else
  step "Cloning repository"
  git clone --branch "$BRANCH" "$REPO_URL" "$REPO_DIR"
  cd "$REPO_DIR"
fi

# ── Build ───────────────────────────────────────────────────────────
step "Building rust_cc_proxy"
if [ "$BUILD_RELEASE" = "1" ]; then
  cargo build --release
else
  cargo build
fi

PROFILE="${BUILD_RELEASE:+release}"
PROFILE="${PROFILE:-debug}"
BIN_SRC="target/$PROFILE/rust_cc_proxy"

step "Building headroom-ffi DLL"
cargo build -p headroom-ffi --release 2>/dev/null || warn "headroom-ffi build skipped (crate may not be in workspace)"

# ── Install ─────────────────────────────────────────────────────────
step "Installing to $INSTALL_DIR"
mkdir -p "$INSTALL_DIR" "$CONFIG_DIR"

cp -f "$BIN_SRC" "$INSTALL_DIR/rust_cc_proxy"
chmod +x "$INSTALL_DIR/rust_cc_proxy"

# Copy headroom DLL if it was built
for dll in target/release/libheadroom_ffi.so target/release/libheadroom_ffi.dylib; do
  if [ -f "$dll" ]; then
    cp -f "$dll" "$INSTALL_DIR/headroom_core.so"
    info "Installed headroom DLL"
    break
  fi
done

# ── PATH check ──────────────────────────────────────────────────────
step "Environment check"
if ! echo "$PATH" | grep -q "$INSTALL_DIR"; then
  warn "Add $INSTALL_DIR to your PATH:"
  echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
  SHELL_RC=""
  case "$SHELL" in
    */zsh) SHELL_RC="$HOME/.zshrc" ;;
    */bash) SHELL_RC="$HOME/.bashrc" ;;
  esac
  if [ -n "$SHELL_RC" ]; then
    if ! grep -q "$INSTALL_DIR" "$SHELL_RC" 2>/dev/null; then
      echo "echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> $SHELL_RC"
    fi
  fi
fi

# ── Verify ──────────────────────────────────────────────────────────
step "Verifying installation"
"$INSTALL_DIR/rust_cc_proxy" --version || warn "Binary may need runtime dependencies"

# ── Done ────────────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}${BOLD}  ✓ Installation complete!${RESET}"
echo ""
echo "  Binary:   $INSTALL_DIR/rust_cc_proxy"
echo "  Config:   $CONFIG_DIR"
echo ""
echo "  Quick start:"
echo "    export DEEPSEEK_API_KEY=sk-..."
echo "    rust_cc_proxy"
echo ""
echo "  Dev mode:"
echo "    rust_cc_proxy --dev"
echo ""
echo "  With compression:"
echo "    COMPRESSION_ENABLED=true rust_cc_proxy"
