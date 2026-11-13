#!/bin/sh
# Write trix's two AUR packages for a release: trix, built from the
# tag's source, and trix-bin, from the release's prebuilt archives. Each
# gets a PKGBUILD and a .SRCINFO. The release workflow runs it and pushes
# the results to aur.archlinux.org.
#
#   packaging/aur.sh 1.0.0 dir-with-the-sha256-files out-dir
#
# The source archive's checksum is fetched from GitHub.
set -eu

version=$1
sums=$2
out=$3
repo="https://github.com/leofle/trix"
desc="A terminal workspace where coding agents live"

sha() {
  file="$sums/trix-$1.tar.gz.sha256"
  [ -s "$file" ] || { echo "missing $file" >&2; exit 1; }
  cut -d' ' -f1 "$file"
}

src_sha=$(curl -fsSL "$repo/archive/refs/tags/v$version.tar.gz" | sha256sum | cut -d' ' -f1)
x86_sha=$(sha x86_64-unknown-linux-gnu)
arm_sha=$(sha aarch64-unknown-linux-gnu)

header="# Maintainer: leofle <2674606+leofle@users.noreply.github.com>
# Written by trix's release workflow (packaging/aur.sh in leofle/trix).
# Edits here are replaced at the next release."

optdepends="optdepends=(
  'git: worktrees, and the branch and diff on each card'
  'libnotify: desktop notifications'
  'pipewire: the chime when an agent finishes'
  'wl-clipboard: copying on Wayland'
)"

srcinfo_optdepends="	optdepends = git: worktrees, and the branch and diff on each card
	optdepends = libnotify: desktop notifications
	optdepends = pipewire: the chime when an agent finishes
	optdepends = wl-clipboard: copying on Wayland"

mkdir -p "$out/trix" "$out/trix-bin"

cat > "$out/trix/PKGBUILD" <<PKGBUILD
$header

pkgname=trix
pkgver=$version
pkgrel=1
pkgdesc="$desc"
arch=('x86_64' 'aarch64')
url="$repo"
license=('MIT')
depends=('gcc-libs' 'glibc')
makedepends=('cargo')
$optdepends
source=("\$pkgname-\$pkgver.tar.gz::\$url/archive/refs/tags/v\$pkgver.tar.gz")
sha256sums=('$src_sha')

prepare() {
  cd "\$pkgname-\$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked --target "\$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd "\$pkgname-\$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_TARGET_DIR=target
  cargo build --frozen --release
}

package() {
  cd "\$pkgname-\$pkgver"
  install -Dm755 target/release/trix "\$pkgdir/usr/bin/trix"
  install -Dm644 LICENSE "\$pkgdir/usr/share/licenses/\$pkgname/LICENSE"
  install -Dm644 README.md "\$pkgdir/usr/share/doc/\$pkgname/README.md"
}
PKGBUILD

cat > "$out/trix/.SRCINFO" <<SRCINFO
pkgbase = trix
	pkgdesc = $desc
	pkgver = $version
	pkgrel = 1
	url = $repo
	arch = x86_64
	arch = aarch64
	license = MIT
	makedepends = cargo
	depends = gcc-libs
	depends = glibc
$srcinfo_optdepends
	source = trix-$version.tar.gz::$repo/archive/refs/tags/v$version.tar.gz
	sha256sums = $src_sha

pkgname = trix
SRCINFO

cat > "$out/trix-bin/PKGBUILD" <<PKGBUILD
$header

pkgname=trix-bin
pkgver=$version
pkgrel=1
pkgdesc="$desc (prebuilt)"
arch=('x86_64' 'aarch64')
url="$repo"
license=('MIT')
depends=('gcc-libs' 'glibc')
$optdepends
provides=('trix')
conflicts=('trix')
source_x86_64=("trix-\$pkgver-x86_64.tar.gz::\$url/releases/download/v\$pkgver/trix-x86_64-unknown-linux-gnu.tar.gz")
source_aarch64=("trix-\$pkgver-aarch64.tar.gz::\$url/releases/download/v\$pkgver/trix-aarch64-unknown-linux-gnu.tar.gz")
sha256sums_x86_64=('$x86_sha')
sha256sums_aarch64=('$arm_sha')

package() {
  cd "trix-\$CARCH-unknown-linux-gnu"
  install -Dm755 trix "\$pkgdir/usr/bin/trix"
  install -Dm644 LICENSE "\$pkgdir/usr/share/licenses/trix/LICENSE"
  install -Dm644 README.md "\$pkgdir/usr/share/doc/trix/README.md"
}
PKGBUILD

cat > "$out/trix-bin/.SRCINFO" <<SRCINFO
pkgbase = trix-bin
	pkgdesc = $desc (prebuilt)
	pkgver = $version
	pkgrel = 1
	url = $repo
	arch = x86_64
	arch = aarch64
	license = MIT
	depends = gcc-libs
	depends = glibc
$srcinfo_optdepends
	provides = trix
	conflicts = trix
	source_x86_64 = trix-$version-x86_64.tar.gz::$repo/releases/download/v$version/trix-x86_64-unknown-linux-gnu.tar.gz
	sha256sums_x86_64 = $x86_sha
	source_aarch64 = trix-$version-aarch64.tar.gz::$repo/releases/download/v$version/trix-aarch64-unknown-linux-gnu.tar.gz
	sha256sums_aarch64 = $arm_sha

pkgname = trix-bin
SRCINFO
