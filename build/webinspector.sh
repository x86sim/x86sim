#!/bin/sh
# WebKit's Web Inspector frontend (WebInspectorUI, as built for WebKitGTK: Main.html, the combined Main.js / Main.css,
# images, workers, en.lproj strings) for `x86sim inspect` (tools/launcher/core/src/share/inspect.rs), so it needs no
# WebKitGTK on the host. Taken from a pinned WebKitGTK binary package (Arch Linux's archive keeps every build; the
# sha256 below is checked, the package is also signed by Arch) by reading the library's GResources: the same files
# WebKitGTK's own inspector loads, already combined and minified by WebKit's build. Building them from WebKit's
# source tree instead needs a WebKit checkout and its Perl/Python resource scripts for the same result.
#   build/webinspector.sh [dest]   dest default tools/launcher/target/webinspector (the checkout's copy; release
#                                  AppImages carry one at usr/share/x86sim/webinspector)
#   WEBINSPECTOR_DL                download cache dir (default: dest's parent/downloads)
# Needs curl, zstd, tar and gresource (glib: libglib2.0-bin on Debian/Ubuntu).
# The result: dest/UserInterface, dest/Localizations, dest/LICENSE (WebInspectorUI's BSD header and the licenses of
# the libraries combined into it: CodeMirror, Esprima, three.js, CSS documentation), dest/VERSION.
set -e
X=$(cd "$(dirname "$0")/.." && pwd)
WK_VER=${WK_VER:-2.54.3-1}
WK_SHA=${WK_SHA:-3ca89124d8e48ef93512beeffa44ff3eeea774c7cfcc5d856c2d43f6bcd3a042}
WK_URL=https://archive.archlinux.org/packages/w/webkit2gtk-4.1/webkit2gtk-4.1-$WK_VER-x86_64.pkg.tar.zst
DEST=${1:-$X/tools/launcher/target/webinspector}
DL=${WEBINSPECTOR_DL:-$(dirname "$DEST")/downloads}
for t in curl zstd tar gresource; do
	command -v $t >/dev/null || { echo "webinspector: $t is needed (gresource: glib, libglib2.0-bin)" >&2; exit 1; }
done
if [ -f "$DEST/VERSION" ] && [ "$(cat "$DEST/VERSION")" = "webkit2gtk-4.1 $WK_VER" ] && [ -f "$DEST/UserInterface/Main.html" ]; then
	echo "  webinspector $WK_VER (already there: $DEST)"
	exit 0
fi
mkdir -p "$DL"
PKG=$DL/webkit2gtk-4.1-$WK_VER.pkg.tar.zst
if [ ! -s "$PKG" ]; then curl -fsSL --retry 3 -o "$PKG.part" "$WK_URL" && mv "$PKG.part" "$PKG"; fi
h=$(sha256sum "$PKG" | cut -d' ' -f1)
[ "$h" = "$WK_SHA" ] || { echo "webinspector: $PKG: sha256 $h, expected $WK_SHA" >&2; exit 1; }
T=$DEST.tmp$$
rm -rf "$T"
mkdir -p "$T/pkg" "$T/ui"
zstd -dc "$PKG" | tar -x -C "$T/pkg" --wildcards 'usr/lib/libwebkit2gtk-4.1.so.0.*' usr/share/licenses/webkit2gtk-4.1/LICENSE
LIB=$(find "$T/pkg/usr/lib" -name 'libwebkit2gtk-4.1.so.0.*' -type f | head -1)
P=/org/webkit/inspector/
n=0
for r in $(gresource list "$LIB" | grep "^$P"); do
	rel=${r#"$P"}
	case "/$rel/" in */../*|*//*) continue ;; esac
	mkdir -p "$T/ui/$(dirname "$rel")"
	gresource extract "$LIB" "$r" > "$T/ui/$rel"
	n=$((n + 1))
done
[ -f "$T/ui/UserInterface/Main.html" ] && [ -f "$T/ui/UserInterface/Main.js" ] \
	|| { echo "webinspector: no Web Inspector frontend in $LIB's resources" >&2; exit 1; }
# inspect.rs patches Main.js's socket URL (wss:// behind HTTPS): make sure this build still has the text it replaces
grep -q '"ws://"+queryParams.ws' "$T/ui/UserInterface/Main.js" \
	|| { echo "webinspector: Main.js has no \"ws://\"+queryParams.ws (the wss:// patch in share/inspect.rs needs updating)" >&2; exit 1; }
{
	echo "WebKit Web Inspector frontend (Source/WebInspectorUI), as built in WebKitGTK $WK_VER"
	echo "(webkit2gtk-4.1 $WK_VER, $WK_URL)."
	echo "Source: https://webkitgtk.org/releases/webkitgtk-${WK_VER%-*}.tar.xz"
	echo
	echo "### Source/WebInspectorUI (UserInterface/Main.js, Main.css, Main.html, Workers, Images, Localizations) ###"
	# WebInspectorUI's own BSD-2-Clause header, as its combined Main.js carries it
	sed -n '1,/\*\//p' "$T/ui/UserInterface/Main.js"
	echo
	# the libraries combined into CodeMirror.js, Esprima.js, Three.js and CSSDocumentation.js (from WebKitGTK's
	# collected license file)
	awk '/^### /{keep = ($0 ~ /^### Source\/WebInspectorUI\//)} keep' "$T/pkg/usr/share/licenses/webkit2gtk-4.1/LICENSE"
} > "$T/ui/LICENSE"
grep -q 'Source/WebInspectorUI/UserInterface/External/CodeMirror' "$T/ui/LICENSE" \
	|| { echo "webinspector: WebKitGTK's license file has no WebInspectorUI sections" >&2; exit 1; }
echo "webkit2gtk-4.1 $WK_VER" > "$T/ui/VERSION"
rm -rf "$DEST"
mkdir -p "$(dirname "$DEST")"
mv "$T/ui" "$DEST"
rm -rf "$T"
echo "  webinspector $WK_VER: $n files, $(du -sh "$DEST" | cut -f1) in $DEST"
