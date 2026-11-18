#!/usr/bin/env bash
# Builds a release and publishes it to your own devices.
#
#   scripts/release.sh 0.1.3 [notes.md] [--github]
#
# 1. Sets the version everywhere (Cargo, the app, Android's version code).
# 2. Builds the Windows installers and the Android APK.
# 3. Signs both with the update key (~/.nexus-signing/update-key, or
#    NEXUS_UPDATE_KEY) and adds them to this computer's NEXUS update store.
#    From there every paired device fetches them, checks the signature and
#    installs (Windows: by itself once its window is closed; Android: after
#    you confirm).
# 4. Copies the files to dist/release-<version>/ and, with --github, creates
#    the GitHub release (the notes file is used for both).
#
# Nothing is committed or pushed: commit the version change yourself.
set -euo pipefail

VERSION="${1:?usage: scripts/release.sh VERSION [notes.md] [--github]}"
NOTES=""
GITHUB=0
for arg in "${@:2}"; do
  case "$arg" in
    --github) GITHUB=1 ;;
    *) NOTES="$arg" ;;
  esac
done
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "version must look like 1.2.3" >&2; exit 1; }

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "== Version $VERSION"
sed -i -E "0,/^version = \"[^\"]+\"/s//version = \"$VERSION\"/" Cargo.toml
sed -i -E "0,/\"version\": \"[^\"]+\"/s//\"version\": \"$VERSION\"/" apps/desktop/package.json apps/desktop/src-tauri/tauri.conf.json
cargo update --workspace --quiet
(cd apps/desktop && npm install --package-lock-only --silent)

echo "== Release tool"
cargo build --release --quiet -p nexus-core --bin nexus-release
TOOL="$ROOT/target/release/nexus-release"
"$TOOL" pubkey >/dev/null   # fails early if the update key is missing

echo "== Windows"
npm run desktop:build
SETUP="target/release/bundle/nsis/NEXUS Cloud_${VERSION}_x64-setup.exe"
MSI="target/release/bundle/msi/NEXUS Cloud_${VERSION}_x64_en-US.msi"

echo "== Android"
bash scripts/build-android.sh aarch64 release
APK="apps/desktop/src-tauri/gen/android/app/build/outputs/apk/arm64/release/app-arm64-release.apk"

OUT="dist/release-$VERSION"
mkdir -p "$OUT"
cp "$SETUP" "$OUT/NEXUS-Cloud_${VERSION}_x64-setup.exe"
cp "$MSI" "$OUT/NEXUS-Cloud_${VERSION}_x64.msi"
cp "$APK" "$OUT/NEXUS-Cloud_${VERSION}_arm64.apk"

echo "== Publishing to your devices"
NOTE_ARGS=()
[[ -n "$NOTES" ]] && NOTE_ARGS=(--notes "$NOTES")
"$TOOL" publish --platform windows-x86_64 --version "$VERSION" \
  --package "$OUT/NEXUS-Cloud_${VERSION}_x64-setup.exe" "${NOTE_ARGS[@]}"
"$TOOL" publish --platform android-aarch64 --version "$VERSION" \
  --package "$OUT/NEXUS-Cloud_${VERSION}_arm64.apk" "${NOTE_ARGS[@]}"

if [[ $GITHUB == 1 ]]; then
  echo "== GitHub release"
  gh release create "v$VERSION" --target "$(git branch --show-current)" --title "NEXUS Cloud $VERSION" \
    ${NOTES:+--notes-file "$NOTES"} \
    "$OUT/NEXUS-Cloud_${VERSION}_x64-setup.exe" "$OUT/NEXUS-Cloud_${VERSION}_x64.msi" "$OUT/NEXUS-Cloud_${VERSION}_arm64.apk"
fi

echo "Done: $OUT"
