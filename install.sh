#!/bin/sh
# Builds the pombo client from source and installs it as `pombo`.
#
#   ./install.sh              install to ~/.local/bin (no root needed)
#   PREFIX=/usr/local sudo -E ./install.sh   install system-wide
#   ./install.sh --uninstall  remove it
set -eu

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
TARGET="$BIN_DIR/pombo"
REPO_DIR="$(cd "$(dirname "$0")" && pwd)"

say() { printf '\033[1;36m==>\033[0m %s\n' "$1"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

if [ "${1:-}" = "--uninstall" ]; then
    if [ -e "$TARGET" ]; then
        rm -f "$TARGET"
        say "removed $TARGET"
    else
        say "nothing to remove at $TARGET"
    fi
    exit 0
fi

[ "$(uname -s)" = "Linux" ] || die "pombo only runs on Linux"

if ! command -v cargo >/dev/null 2>&1; then
    [ -x "$HOME/.cargo/bin/cargo" ] && PATH="$HOME/.cargo/bin:$PATH"
fi
command -v cargo >/dev/null 2>&1 || die "Rust is not installed. Install it with:
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
then open a new terminal and run ./install.sh again."

command -v cc >/dev/null 2>&1 || die "a C compiler is needed to build the TLS library.
    Debian/Ubuntu: sudo apt install build-essential
    Fedora:        sudo dnf install gcc
    Arch:          sudo pacman -S base-devel"

say "building pombo (first build takes a minute or two)"
cd "$REPO_DIR"
cargo build --release --locked -p pombo-irc --bin pombo \
    || die "build failed. If your Rust is old, run 'rustup update' and try again."

mkdir -p "$BIN_DIR"
install -m 755 "$REPO_DIR/target/release/pombo" "$TARGET"
say "installed $TARGET"

case ":$PATH:" in
    *":$BIN_DIR:"*)
        say "done. Run: pombo"
        ;;
    *)
        say "done, but $BIN_DIR is not in your PATH. Add this line to your ~/.bashrc or ~/.zshrc:"
        printf '    export PATH="%s:$PATH"\n' "$BIN_DIR"
        say "then open a new terminal and run: pombo"
        ;;
esac
