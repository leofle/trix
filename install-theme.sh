#!/bin/sh
# Install a trix theme: copy its colors.toml to ~/.config/trix/colors.toml,
# where every running trix picks it up within a second.
#
#   ./install-theme.sh ristretto       a theme from themes/
#   ./install-theme.sh path/to/theme   an Omarchy theme folder, or a colors.toml
#   ./install-theme.sh                 list the themes in themes/
set -eu

themes=$(cd "$(dirname "$0")" && pwd)/themes

# The same folder trix reads: $XDG_CONFIG_HOME when it's absolute.
case ${XDG_CONFIG_HOME:-} in
    /*) dest=$XDG_CONFIG_HOME/trix/colors.toml ;;
    *) dest=$HOME/.config/trix/colors.toml ;;
esac

show() {
    case $1 in
        "$HOME"/*) printf '~%s' "${1#"$HOME"}" ;;
        *) printf '%s' "$1" ;;
    esac
}

# A colors.toml's colors: its lines but for comments and blank ones, so
# Omarchy's copy of a theme here, without our first line, is that theme.
colors() {
    grep -v -e '^[[:space:]]*#' -e '^[[:space:]]*$' "$1" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//'
}
same() {
    [ "$(colors "$1")" = "$(colors "$2")" ]
}

usage() {
    echo "usage: $0 <theme>"
    echo
    echo "<theme> is one of these, an Omarchy theme folder, or a colors.toml:"
    for d in "$themes"/*/; do
        if [ -f "$d/colors.toml" ]; then
            printf '  %s\n' "$(basename "$d")"
        fi
    done
}

case $# in
    0) usage; exit 0 ;;
    1) ;;
    *) usage >&2; exit 2 ;;
esac
case $1 in
    -h | --help) usage; exit 0 ;;
esac

if [ -f "$1" ]; then
    src=$1
    name=$(show "$1")
elif [ -f "$1/colors.toml" ]; then
    src=$1/colors.toml
    name=$(show "$1")
else
    # "Tokyo Night" and "tokyo_night" both mean tokyo-night.
    name=$(printf '%s' "$1" | tr 'A-Z _' 'a-z--')
    src=$themes/$name/colors.toml
    if [ ! -f "$src" ]; then
        echo "install-theme: no theme called '$1'" >&2
        echo >&2
        usage >&2
        exit 1
    fi
fi

if ! grep -q '^[[:space:]]*background[[:space:]]*=' "$src"; then
    echo "install-theme: $(show "$src") has no 'background = ...': not a colors.toml?" >&2
    exit 1
fi

if [ -f "$dest" ] && same "$src" "$dest"; then
    echo "$name is already installed: $(show "$dest")"
    exit 0
fi

# A colors.toml that isn't one of ours was made or edited by hand: keep it.
if [ -f "$dest" ]; then
    ours=no
    for f in "$themes"/*/colors.toml; do
        if same "$f" "$dest"; then
            ours=yes
            break
        fi
    done
    if [ $ours = no ]; then
        backup=$dest.$(date +%Y%m%d-%H%M%S).bak
        cp "$dest" "$backup"
        echo "Saved your previous colors.toml as $(show "$backup")"
    fi
fi

mkdir -p "$(dirname "$dest")"
cp "$src" "$dest"
echo "Installed $name: $(show "$dest")"
echo "Every running trix restyles within a second, its panes too."
echo "Agents take it when they start: restart one (alt+o, restart) whose colors don't match."
if [ -n "${TRIX_THEME:-}" ]; then
    echo "Note: TRIX_THEME is set ($(show "$TRIX_THEME")), and a trix started with it uses that file instead."
    echo "Remove it from your shell profile, then run 'trix kill' and start trix again."
fi
if [ -f "$HOME/.local/state/omarchy/current/theme/colors.toml" ] || [ -f "$HOME/.config/omarchy/current/theme/colors.toml" ]; then
    echo "trix now uses this instead of your Omarchy theme. To follow Omarchy again: rm $(show "$dest")"
fi
