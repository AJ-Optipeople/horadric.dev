#!/bin/sh
# Installs Horadric on a Mac, or a Horadric node on Linux, for this user,
# with no admin rights:
#
#   curl -fsSL https://horadric.dev/install.sh | sh
#
# On a Mac: downloads the newest release, then lets Horadric install
# itself: the app in ~/Applications, `horadric` linked into ~/.local/bin,
# opening at login, and the Claude Code hooks. A download by curl is not
# quarantined, so Gatekeeper does not stop an app that is not notarized.
#
# On Linux: puts `horadric` in ~/.local/bin and runs `horadric node
# install`: a systemd user service, kept running logged out, and the
# Claude Code hooks. A Windows Horadric then reaches it over ssh.
set -eu

releases="https://github.com/Mopra/horadric.dev/releases/latest/download"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

linux() {
	case "$(uname -m)" in
	x86_64 | amd64) ;;
	*)
		echo "The Horadric node is built for x86_64 Linux only so far."
		exit 1
		;;
	esac
	url="${HORADRIC_DOWNLOAD:-$releases/Horadric-linux-x86_64.tar.gz}"
	echo "Downloading the Horadric node..."
	curl -fsSL "$url" -o "$tmp/horadric.tar.gz"
	tar -xzf "$tmp/horadric.tar.gz" -C "$tmp"
	bin="$HOME/.local/bin"
	mkdir -p "$bin"
	# Moved into place rather than written over, so a node running the
	# old binary keeps running until the service restarts it.
	install -m 755 "$tmp/horadric" "$bin/horadric.new"
	mv -f "$bin/horadric.new" "$bin/horadric"
	"$bin/horadric" node install
	systemctl --user restart horadric.service || true
	case ":$PATH:" in
	*":$bin:"*) ;;
	*) echo "$bin is not on your PATH yet. Add it, so \`ssh <this host> horadric node --stdio\` finds it." ;;
	esac
}

case "$(uname -s)" in
Darwin) ;;
Linux)
	linux
	exit 0
	;;
*)
	echo "This installer is for macOS and Linux. Horadric for Windows is at https://horadric.dev"
	exit 1
	;;
esac

url="${HORADRIC_DOWNLOAD:-$releases/Horadric-macos.tar.gz}"

echo "Downloading Horadric..."
curl -fsSL "$url" -o "$tmp/Horadric-macos.tar.gz"
tar -xzf "$tmp/Horadric-macos.tar.gz" -C "$tmp"

if nc -z 127.0.0.1 43117 2>/dev/null; then
	echo "Horadric is running. Quit it from its menu bar icon, keeping the sessions running, then run this again."
	exit 1
fi

"$tmp/Horadric.app/Contents/MacOS/horadric" install
