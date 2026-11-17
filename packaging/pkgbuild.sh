#!/bin/sh
# Print a PKGBUILD that packages a trix release for Arch Linux, from that
# release's .sha256 files. It repackages the release's Linux archives
# rather than building from source, so the package holds the same binary
# as the tarball and Homebrew. The release workflow runs it to build the
# pacman packages it attaches to the release.
#
#   packaging/pkgbuild.sh 1.0.0 dir-with-the-sha256-files > PKGBUILD
set -eu

version=$1
dir=$2

sha() {
  file="$dir/trix-$1.tar.gz.sha256"
  [ -s "$file" ] || { echo "missing $file" >&2; exit 1; }
  cut -d' ' -f1 "$file"
}

# Quoted heredoc: nothing in it is expanded here, so makepkg sees the
# variables. The version and checksums go in with sed below.
cat <<'PKGBUILD' | sed -e "s/@VERSION@/$version/" \
  -e "s/@SHA_X86_64@/$(sha x86_64-unknown-linux-gnu)/" \
  -e "s/@SHA_AARCH64@/$(sha aarch64-unknown-linux-gnu)/"
# Written by trix's release workflow (packaging/pkgbuild.sh in
# leofle/trix). Build it with makepkg, or install the package attached
# to the release.
pkgname=trix
pkgver=@VERSION@
pkgrel=1
pkgdesc="Terminal workspace where coding agents live"
arch=('x86_64' 'aarch64')
url="https://github.com/leofle/trix"
license=('MIT')
depends=('glibc' 'gcc-libs')
optdepends=(
  'git: worktrees, branches and diffs on agent cards'
  'libnotify: desktop notifications'
  'pipewire-audio: the chime when an agent finishes'
)
options=('!strip' '!debug')
_base="https://github.com/leofle/trix/releases/download/v$pkgver"
source_x86_64=("$_base/trix-x86_64-unknown-linux-gnu.tar.gz")
source_aarch64=("$_base/trix-aarch64-unknown-linux-gnu.tar.gz")
sha256sums_x86_64=('@SHA_X86_64@')
sha256sums_aarch64=('@SHA_AARCH64@')

package() {
  cd "trix-$CARCH-unknown-linux-gnu"
  install -Dm755 trix "$pkgdir/usr/bin/trix"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"
}
PKGBUILD
