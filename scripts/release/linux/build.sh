#!/usr/bin/env bash
# Builds the Linux release packages (.deb, .AppImage) in an Ubuntu 22.04 container
# and checks that nothing in them needs a glibc newer than 2.35.
#
#   scripts/release/linux/build.sh            # output: release/linux/
#
# Needs Docker (user in the docker group). The working tree is copied as it is,
# uncommitted changes included. Caches (cargo, npm, target) live in Docker volumes
# named termworker-*; `docker volume rm` them to start clean.
set -euo pipefail

MAX_GLIBC=2.35
IMAGE=termworker-build:ubuntu22.04

if [[ "${1:-}" != "--inside" ]]; then
    here=$(cd "$(dirname "$0")" && pwd)
    repo=$(cd "$here/../../.." && pwd)
    out="$repo/release/linux"
    mkdir -p "$out"

    docker build -t "$IMAGE" --build-arg UID="$(id -u)" --build-arg GID="$(id -g)" "$here"
    docker run --rm \
        -v "$repo:/src:ro" \
        -v "$out:/out" \
        -v termworker-work:/home/builder/work \
        -v termworker-cargo:/home/builder/.cargo/registry \
        -v termworker-cache:/home/builder/.cache \
        -e APPIMAGE_EXTRACT_AND_RUN=1 \
        "$IMAGE" bash /src/scripts/release/linux/build.sh --inside
    echo
    ls -l "$out"
    exit 0
fi

# ---- inside the container ----
rsync -a --delete \
    --exclude=.git --exclude=node_modules --exclude=/dist --exclude=/release \
    --exclude=src-tauri/target --exclude=src-tauri/gen/schemas \
    /src/ ./
bundle=src-tauri/target/release/bundle
# Packages of an earlier version stay in the cached target folder otherwise.
rm -rf /home/builder/work/src-tauri/target/release/bundle
npm ci --no-audit --no-fund
npx tauri build --bundles deb appimage

deb=$(ls "$bundle"/deb/*.deb)
appimage=$(ls "$bundle"/appimage/*.AppImage)

# Highest GLIBC_x.y symbol version a file asks for (empty if none).
glibc_of() { objdump -T "$1" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -1 || true; }

fail=0
check() {
    local f=$1 v
    v=$(glibc_of "$f")
    [[ -n "$v" ]] || return 0
    if [[ "$(printf '%s\n%s\n' "$v" "$MAX_GLIBC" | sort -V | tail -1)" != "$MAX_GLIBC" ]]; then
        echo "  TOO NEW  glibc $v  ${f#./}"
        fail=1
    fi
}

echo "== glibc check (max $MAX_GLIBC)"
echo "  binary: glibc $(glibc_of src-tauri/target/release/termworker)"
check src-tauri/target/release/termworker
while IFS= read -r -d '' f; do
    if file -b "$f" | grep -q '^ELF'; then check "$f"; fi
done < <(find "$bundle"/appimage/*.AppDir -type f -print0)
[[ $fail -eq 0 ]] || { echo "glibc check failed"; exit 1; }
echo "  ok: binary and every ELF file in the AppImage"

# ---- notices for the libraries the AppImage carries ----
# The AppImage includes Ubuntu's WebKitGTK, GTK, GLib and their dependencies, many under
# the GNU LGPL. Put inside it: which package and version every bundled file came from,
# the packages' copyright files, the licence texts and where to get the source. Fails
# when a bundled file cannot be traced to an installed package.
appdir=$(ls -d "$bundle"/appimage/*.AppDir)
version=$(node -p "require('./src-tauri/tauri.conf.json').version")
doc="$appdir/usr/share/doc/termworker"

# GTK's TIFF image loader pulls in libtiff and, through it, libjbig (JBIG-KIT), which is
# GPL-2+. TermWorker never opens TIFF images, so the AppImage leaves all three out (and
# no GPL library is shipped). The .deb uses the system's libraries and is not affected.
echo "== leave out TIFF"
loaders="$appdir/usr/lib/gdk-pixbuf-2.0/2.10.0"
rm -f "$loaders"/loaders/libpixbufloader-tiff.so "$appdir"/usr/lib/libtiff.so.* "$appdir"/usr/lib/libjbig.so.*
rm -rf "$appdir"/usr/share/doc/libtiff[0-9]* "$appdir"/usr/share/doc/libjbig[0-9]*
# linuxdeploy also links each loader into usr/lib: drop links left pointing at nothing.
find "$appdir" -xtype l -delete
awk 'BEGIN { RS = ""; ORS = "\n\n" } !/^"libpixbufloader-tiff\.so"/' "$loaders/loaders.cache" > "$loaders/loaders.cache.new"
mv "$loaders/loaders.cache.new" "$loaders/loaders.cache"
! grep -q tiff "$loaders/loaders.cache" || { echo "  the TIFF loader is still in loaders.cache"; exit 1; }
while IFS= read -r -d '' f; do
    file -b "$f" | grep -q '^ELF' || continue
    if objdump -p "$f" 2>/dev/null | grep -qE 'NEEDED +lib(tiff|jbig)\.so'; then
        echo "  still needs libtiff/libjbig: ${f#"$appdir"/}"
        exit 1
    fi
done < <(find "$appdir" -type f -print0)
echo "  ok"

echo "== bundled libraries"
mkdir -p "$doc/licenses"

# Package ("name:arch") that installed a system file; tries /lib and /usr/lib (usrmerge).
owner_of() {
    local p out
    for p in "$1" "${1/#\/usr\/lib\//\/lib\/}" "${1/#\/lib\//\/usr\/lib\/}"; do
        out=$(dpkg -S "$p" 2>/dev/null | head -1) && [[ -n $out ]] && { echo "${out%%: *}"; return 0; }
    done
    return 1
}
# The system file an AppDir file was copied from: linuxdeploy flattens libraries into
# usr/lib and keeps plugin folders (gio/modules, gtk-3.0, gdk-pixbuf-2.0, webkit2gtk-4.1).
# The real file comes first: an unversioned name (libgio-2.0.so) is a -dev symlink.
package_of() {
    local rel=${1#usr/lib/} c
    for c in "/usr/lib/x86_64-linux-gnu/$rel" "/lib/x86_64-linux-gnu/$rel" "/usr/lib/$rel"; do
        [[ -e $c ]] || continue
        owner_of "$(readlink -f "$c")" && return 0
        owner_of "$c" && return 0
    done
    return 1
}

declare -A files_of=()
unmapped=0
while IFS= read -r -d '' f; do
    file -b "$f" | grep -q '^ELF' || continue
    rel=${f#"$appdir"/}
    case $rel in
        usr/bin/termworker | AppRun | AppRun.wrapped) continue ;; # ours, and linuxdeploy's AppRun (MIT)
    esac
    if pkg=$(package_of "$rel"); then
        files_of[$pkg]+="    $rel"$'\n'
    else
        echo "  NOT FROM A PACKAGE: $rel"
        unmapped=1
    fi
done < <(find "$appdir" -type f -print0 | sort -z)
[[ $unmapped -eq 0 ]] || { echo "bundled-library check failed: add the file's source to package_of()"; exit 1; }

# Every bundled package must have been reviewed for its licence (bundled-packages.txt):
# a new dependency fails the build until someone checks it is not GPL-only.
reviewed=$(grep -v '^#' scripts/release/linux/bundled-packages.txt | awk 'NF { print $1 }')
not_reviewed=0
for pkg in "${!files_of[@]}"; do
    grep -qxF "${pkg%%:*}" <<<"$reviewed" && continue
    echo "  NOT REVIEWED: ${pkg%%:*} ($(dpkg-query -W -f='${source:Package}' "$pkg"))"
    not_reviewed=1
done
[[ $not_reviewed -eq 0 ]] || {
    echo "Check the licence of each package above (/usr/share/doc/<package>/copyright; the library,"
    echo "not just the source package). If it is not GPL-only, add it to bundled-packages.txt."
    exit 1
}

list="$doc/BUNDLED-LIBRARIES.txt"
{
    echo "TermWorker $version for Linux (AppImage): bundled libraries"
    echo
    echo "TermWorker itself is free software under MIT OR Apache-2.0. So that it runs without"
    echo "installing anything, this AppImage also carries the shared libraries listed below."
    echo "They were copied from Ubuntu 22.04 LTS (\"jammy\") packages; only their library search"
    echo "path (RPATH) was changed. Each keeps its own licence. Many are under the GNU LGPL: you"
    echo "may replace any of them with your own build (run the AppImage with --appimage-extract,"
    echo "replace the file under squashfs-root/usr/lib and start squashfs-root/AppRun)."
    echo
    echo "Licences:     usr/share/doc/<package>/copyright inside this AppImage, and the texts in"
    echo "              usr/share/doc/termworker/licenses/ (LGPL-2, LGPL-2.1, LGPL-3, GPL-2, GPL-3)."
    echo "Source code:  the Ubuntu jammy source packages listed below, from https://packages.ubuntu.com"
    echo "              (or: apt-get source <source package>=<version>). DHPLab Consulting"
    echo "              (https://dhplab.com) will also give anyone a copy of that source on request,"
    echo "              for no more than the cost of providing it, for at least three years after"
    echo "              this release."
    echo "Also bundled: the AppImage runtime and AppRun from the AppImage and linuxdeploy projects (MIT)."
    echo
    echo "Package (version) - source package (version)"
    echo "    files"
    echo
    for pkg in $(printf '%s\n' "${!files_of[@]}" | sort); do
        dpkg-query -W -f='${Package} (${Version}) - ${source:Package} (${source:Version})\n' "$pkg"
        printf '%s\n' "${files_of[$pkg]}"
    done
} > "$list"

for pkg in "${!files_of[@]}"; do
    name=${pkg%%:*}
    dest="$appdir/usr/share/doc/$name/copyright"
    [[ -e $dest ]] && continue
    src=/usr/share/doc/$name/copyright
    [[ -e $src ]] || { echo "  no copyright file for $name"; exit 1; }
    mkdir -p "$(dirname "$dest")"
    cp "$src" "$dest"
done
for l in LGPL-2 LGPL-2.1 LGPL-3 GPL-2 GPL-3; do
    cp "/usr/share/common-licenses/$l" "$doc/licenses/$l"
done
echo "  ${#files_of[@]} packages, $(grep -c '^    usr/' "$list") files: usr/share/doc/termworker/BUNDLED-LIBRARIES.txt"

# Pack the AppDir again with the same tool tauri used (linuxdeploy's AppImage plugin).
echo "== repack AppImage"
plugin=$HOME/.cache/tauri/linuxdeploy-plugin-appimage.AppImage
[[ -x $plugin ]] || { echo "  $plugin not found"; exit 1; }
out_dir=$(cd "$(dirname "$appimage")" && pwd)
appdir_abs=$(cd "$appdir" && pwd)
name=$(basename "$appimage")
rm -f "$appimage"
repack_log=$(mktemp)
if ! (cd "$out_dir" && ARCH=x86_64 LDAI_OUTPUT="$name" OUTPUT="$name" "$plugin" --appdir "$appdir_abs") >"$repack_log" 2>&1 ||
    [[ ! -f $appimage ]]; then
    cat "$repack_log"
    echo "  repacking did not produce $name"
    exit 1
fi
echo "  $name $(du -h "$appimage" | cut -f1)"

rm -f /out/*.deb /out/*.AppImage
cp "$deb" "$appimage" /out/
cd /out && sha256sum ./*.deb ./*.AppImage > SHA256SUMS
