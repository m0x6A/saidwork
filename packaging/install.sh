#!/bin/sh
# Install saidwork from the release tarball: pulls in libpulse and CA certificates with
# the system package manager, then copies the binary and its shared files. Run from the unpacked tarball directory.
#
#   ./install.sh            # binary to ~/.local/bin
#   sudo ./install.sh       # binary to /usr/local/bin
#   PREFIX=/opt/saidwork ./install.sh
#   SKIP_DEPS=1 ./install.sh
set -eu

here=$(cd "$(dirname "$0")" && pwd)

if [ "$(id -u)" -eq 0 ]; then
    sudo=""
    prefix=${PREFIX:-/usr/local}
else
    sudo="sudo"
    prefix=${PREFIX:-$HOME/.local}
fi

have() { command -v "$1" >/dev/null 2>&1; }

# Install packages with whichever package manager exists; the arguments are the
# package lists for dnf, apt, pacman and zypper.
install_deps() {
    if have dnf; then
        $sudo dnf install -y $1
    elif have apt-get; then
        $sudo apt-get update && $sudo apt-get install -y $2
    elif have pacman; then
        $sudo pacman -S --needed --noconfirm $3
    elif have zypper; then
        $sudo zypper --non-interactive install $4
    else
        echo "warning: unknown package manager; install these yourself:$2" >&2
    fi
}

have_libpulse() {
    have ldconfig && ldconfig -p 2>/dev/null | grep -q 'libpulse-simple\.so\.0'
}

# Without CA certificates saidwork cannot make any HTTP request.
have_ca_certs() {
    for f in /etc/ssl/certs/ca-certificates.crt /etc/pki/tls/certs/ca-bundle.crt \
        /etc/ssl/ca-bundle.pem /etc/ssl/cert.pem; do
        [ -s "$f" ] && return 0
    done
    return 1
}

if [ -n "${SKIP_DEPS:-}" ]; then
    echo "skipping dependency install"
else
    dnf="" apt="" pacman="" zypper=""
    if ! have_libpulse; then
        dnf="pulseaudio-libs" apt="libpulse0" pacman="libpulse" zypper="libpulse0"
    fi
    if ! have_ca_certs; then
        dnf="$dnf ca-certificates" apt="$apt ca-certificates"
        pacman="$pacman ca-certificates" zypper="$zypper ca-certificates"
    fi
    if [ -n "$apt" ]; then
        echo "installing dependencies: $apt"
        install_deps "$dnf" "$apt" "$pacman" "$zypper"
    else
        echo "dependencies already installed"
    fi
fi

install -Dm755 "$here/bin/saidwork" "$prefix/bin/saidwork"
# Shipped resources; saidwork finds them through `{share}/…` in its configuration.
(cd "$here/share/saidwork" && find . -type f) | while read -r f; do
    install -Dm644 "$here/share/saidwork/$f" "$prefix/share/saidwork/$f"
done
echo "installed $prefix/bin/saidwork"

case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *) echo "note: $prefix/bin is not on your PATH" ;;
esac

echo "next: saidwork config init     # or --preset whisper-ollama / openai"
echo "      saidwork doctor          # says what else to install for your configuration"
