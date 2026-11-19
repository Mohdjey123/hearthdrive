#!/usr/bin/env bash
# Starts a NEXUS release.
#
#   scripts/release.sh 1.2.3 [notes.md]            a stable release
#   scripts/release.sh 1.2.3 [notes.md] --beta     a beta
#   scripts/release.sh 1.2.3 [notes.md] --local    build here instead
#
# Normally (the working tree must be clean, on the branch to release from):
# 1. Sets the version everywhere (Cargo, the app, Android's version code)
#    and keeps the notes as docs/releases/<version>.md.
# 2. Commits that as "Release <version>", tags it v<version> (a beta:
#    v<version>-beta) and pushes the branch and the tag.
# 3. The tag starts the Release workflow (.github/workflows/release.yml) on
#    GitHub, which builds Windows and Android, signs them with the update key
#    kept in the repository's secrets, and publishes the GitHub release that
#    every device looks at. Nothing is built or signed on this computer.
#
# A beta reaches only devices set to "Get beta versions". When it is good,
# make it stable without building again:
#   gh workflow run promote.yml -f version=1.2.3
#
# --local builds Windows and Android on this computer instead, signs them
# with ~/.nexus-signing/update-key (or NEXUS_UPDATE_KEY) and adds them to
# this computer's NEXUS update store, from where paired devices fetch them.
# Nothing goes to GitHub, and nothing is committed. For testing, or when
# GitHub is out of reach.
set -euo pipefail

VERSION="${1:?usage: scripts/release.sh VERSION [notes.md] [--beta] [--local]}"
NOTES=""
BETA=0
LOCAL=0
for arg in "${@:2}"; do
  case "$arg" in
    --beta) BETA=1 ;;
    --local) LOCAL=1 ;;
    --*) echo "unknown option: $arg" >&2; exit 2 ;;
    *) NOTES="$arg" ;;
  esac
done
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "version must look like 1.2.3" >&2; exit 1; }
[[ -z "$NOTES" || -f "$NOTES" ]] || { echo "no notes file at $NOTES" >&2; exit 1; }
# Betas come only from GitHub: paired devices never pass them on.
[[ $BETA == 1 && $LOCAL == 1 ]] && { echo "--beta and --local do not go together: betas are published on GitHub" >&2; exit 1; }
TAG="v$VERSION"
[[ $BETA == 1 ]] && TAG="v$VERSION-beta"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ $LOCAL == 0 ]]; then
  if [[ -n "$(git status --porcelain)" ]]; then
    echo "commit or stash your changes first: the release is built from what is committed" >&2
    exit 1
  fi
  BRANCH="$(git branch --show-current)"
  [[ -n "$BRANCH" ]] || { echo "check out the branch to release from first" >&2; exit 1; }
  git fetch -q origin --tags
  if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    echo "the tag $TAG already exists" >&2
    exit 1
  fi
fi

echo "== Version $VERSION"
sed -i -E "0,/^version = \"[^\"]+\"/s//version = \"$VERSION\"/" Cargo.toml
sed -i -E "0,/\"version\": \"[^\"]+\"/s//\"version\": \"$VERSION\"/" apps/desktop/package.json apps/desktop/src-tauri/tauri.conf.json
cargo update --workspace --quiet
(cd apps/desktop && npm install --package-lock-only --silent)

if [[ $LOCAL == 0 ]]; then
  if [[ -n "$NOTES" ]]; then
    mkdir -p docs/releases
    [[ "$NOTES" -ef "docs/releases/$VERSION.md" ]] || cp "$NOTES" "docs/releases/$VERSION.md"
    git add "docs/releases/$VERSION.md"
  fi
  git add Cargo.toml Cargo.lock apps/desktop/package.json apps/desktop/src-tauri/tauri.conf.json package-lock.json
  if [[ $BETA == 1 ]]; then git commit -q -m "Release $VERSION beta"; else git commit -q -m "Release $VERSION"; fi
  git tag -a "$TAG" -m "NEXUS Cloud $VERSION$([[ $BETA == 1 ]] && echo ' beta')"
  echo "== Pushing $BRANCH and $TAG"
  git push -q origin "$BRANCH" "refs/tags/$TAG"
  echo "GitHub is building $TAG now. Follow it with:"
  echo "  gh run watch \$(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
  [[ $BETA == 1 ]] && echo "When it is good: gh workflow run promote.yml -f version=$VERSION"
  exit 0
fi

NOTE_ARGS=()
[[ -n "$NOTES" ]] && NOTE_ARGS=(--notes "$NOTES")

echo "== Release tool"
cargo build --release --quiet -p nexus-core --bin nexus-release
TOOL="$ROOT/target/release/nexus-release"
"$TOOL" pubkey >/dev/null   # fails early if the update key is missing

OUT="dist/release-$VERSION"
mkdir -p "$OUT"

echo "== Windows"
npm run desktop:build
cp "target/release/bundle/nsis/NEXUS Cloud_${VERSION}_x64-setup.exe" "$OUT/NEXUS-Cloud_${VERSION}_x64-setup.exe"
"$TOOL" publish --platform windows-x86_64 --version "$VERSION" \
  --package "$OUT/NEXUS-Cloud_${VERSION}_x64-setup.exe" "${NOTE_ARGS[@]}"

echo "== Android"
bash scripts/build-android.sh aarch64 release
cp "apps/desktop/src-tauri/gen/android/app/build/outputs/apk/arm64/release/app-arm64-release.apk" "$OUT/NEXUS-Cloud_${VERSION}_arm64.apk"
"$TOOL" publish --platform android-aarch64 --version "$VERSION" \
  --package "$OUT/NEXUS-Cloud_${VERSION}_arm64.apk" "${NOTE_ARGS[@]}"

echo "Done: $OUT (published to this computer's NEXUS; the version change is not committed)"
