#!/bin/sh
# Print trix's Homebrew formula for a release, from that release's
# .sha256 files. The release workflow runs it to update the formula in
# leofle/homebrew-trix.
#
#   packaging/homebrew-formula.sh 1.0.0 dir-with-the-sha256-files
set -eu

version=$1
dir=$2
base="https://github.com/leofle/trix/releases/download/v$version"

sha() {
  file="$dir/trix-$1.tar.gz.sha256"
  [ -s "$file" ] || { echo "missing $file" >&2; exit 1; }
  cut -d' ' -f1 "$file"
}

cat <<FORMULA
# Written by trix's release workflow (packaging/homebrew-formula.sh in
# leofle/trix). Edits here are replaced at the next release.
class Trix < Formula
  desc "Terminal workspace where coding agents live"
  homepage "https://github.com/leofle/trix"
  license "MIT"

  on_macos do
    on_arm do
      url "$base/trix-aarch64-apple-darwin.tar.gz"
      sha256 "$(sha aarch64-apple-darwin)"
    end
    on_intel do
      url "$base/trix-x86_64-apple-darwin.tar.gz"
      sha256 "$(sha x86_64-apple-darwin)"
    end
  end

  on_linux do
    on_arm do
      url "$base/trix-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "$(sha aarch64-unknown-linux-gnu)"
    end
    on_intel do
      url "$base/trix-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "$(sha x86_64-unknown-linux-gnu)"
    end
  end

  def install
    bin.install "trix"
  end

  test do
    assert_match "trix #{version}", shell_output("#{bin}/trix --version")
  end
end
FORMULA
