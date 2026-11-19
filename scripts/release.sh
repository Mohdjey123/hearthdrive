#!/usr/bin/env bash
# Builds a release and publishes it to your devices.
#
#   scripts/release.sh 0.1.3 [notes.md] [--github]
#
# 1. Sets the version everywhere (Cargo, the app, Android's version code).
# 2. Builds the Windows installers (and, without --github, the Android APK).
# 3. Signs them with the update key (~/.nexus-signing/update-key, or
#    NEXUS_UPDATE_KEY) and adds them to this computer's NEXUS update store.
#    From there every paired device fetches them, checks the signature and
#    installs (Windows: by itself once its window is closed; Android: after
#    you confirm).
# 4. Copies the files to dist/release-<version>/.
#
# With --github (the working tree must be clean before you start):
# 5. Creates the release on the public releases repository with the Windows
#    installers and their signed update description. Every device looks
#    there too, so devices that cannot reach this computer still update.
# 6. Commits the version change (and the notes, as docs/releases/<v>.md),
#    tags it v<version> and pushes the tag. The tag starts the Android
#    release workflow (.github/workflows/android-release.yml), which builds
#    and signs the APK and adds it to the same release.
#    The branch itself is not pushed: push it yourself.
set -euo pipefail

RELEASES_REPO="${NEXUS_RELEASES_REPO:-Mohdjey123/nexus-cloud-releases}"

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

if [[ $GITHUB == 1 ]]; then
  command -v gh >/dev/null || { echo "--github needs the GitHub CLI (gh), signed in" >&2; exit 1; }
  if [[ -n "$(git status --porcelain)" ]]; then
    echo "--github builds what is committed: commit or stash your changes first" >&2
    exit 1
  fi
  if git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
    echo "the tag v$VERSION already exists" >&2
    exit 1
  fi
fi

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

OUT="dist/release-$VERSION"
mkdir -p "$OUT"
cp "$SETUP" "$OUT/NEXUS-Cloud_${VERSION}_x64-setup.exe"
cp "$MSI" "$OUT/NEXUS-Cloud_${VERSION}_x64.msi"

NOTE_ARGS=()
[[ -n "$NOTES" ]] && NOTE_ARGS=(--notes "$NOTES")

echo "== Publishing to your devices"
"$TOOL" publish --platform windows-x86_64 --version "$VERSION" \
  --package "$OUT/NEXUS-Cloud_${VERSION}_x64-setup.exe" "${NOTE_ARGS[@]}"

if [[ $GITHUB == 0 ]]; then
  echo "== Android"
  bash scripts/build-android.sh aarch64 release
  APK="apps/desktop/src-tauri/gen/android/app/build/outputs/apk/arm64/release/app-arm64-release.apk"
  cp "$APK" "$OUT/NEXUS-Cloud_${VERSION}_arm64.apk"
  "$TOOL" publish --platform android-aarch64 --version "$VERSION" \
    --package "$OUT/NEXUS-Cloud_${VERSION}_arm64.apk" "${NOTE_ARGS[@]}"
  echo "Done: $OUT"
  exit 0
fi

echo "== GitHub release on $RELEASES_REPO"
"$TOOL" sign --platform windows-x86_64 --version "$VERSION" \
  --package "$OUT/NEXUS-Cloud_${VERSION}_x64-setup.exe" --out "$OUT/nexus-update-windows-x86_64.json" "${NOTE_ARGS[@]}"
(cd "$OUT" && sha256sum "NEXUS-Cloud_${VERSION}_x64-setup.exe" > "NEXUS-Cloud_${VERSION}_x64-setup.exe.sha256")
GH_NOTES=(--notes "NEXUS Cloud $VERSION")
[[ -n "$NOTES" ]] && GH_NOTES=(--notes-file "$NOTES")
gh release create "v$VERSION" -R "$RELEASES_REPO" --target main --title "NEXUS Cloud $VERSION" "${GH_NOTES[@]}" \
  "$OUT/NEXUS-Cloud_${VERSION}_x64-setup.exe" "$OUT/NEXUS-Cloud_${VERSION}_x64-setup.exe.sha256" \
  "$OUT/NEXUS-Cloud_${VERSION}_x64.msi" "$OUT/nexus-update-windows-x86_64.json"

echo "== Tag v$VERSION (starts the Android build)"
if [[ -n "$NOTES" ]]; then
  mkdir -p docs/releases
  cp "$NOTES" "docs/releases/$VERSION.md"
  git add "docs/releases/$VERSION.md"
fi
git add Cargo.toml Cargo.lock apps/desktop/package.json apps/desktop/src-tauri/tauri.conf.json package-lock.json
git commit -q -m "Release $VERSION"
git tag -a "v$VERSION" -m "NEXUS Cloud $VERSION"
git push -q origin "v$VERSION"

echo "Done: $OUT"
echo "The Android app is being built on GitHub: gh run list --workflow android-release.yml"
echo "Push the release commit when you are ready: git push"
