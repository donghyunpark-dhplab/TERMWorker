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
    --exclude=.git --exclude=node_modules --exclude=dist --exclude=release \
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

rm -f /out/*.deb /out/*.AppImage
cp "$deb" "$appimage" /out/
cd /out && sha256sum ./*.deb ./*.AppImage > SHA256SUMS
