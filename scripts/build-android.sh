#!/usr/bin/env bash
# Builds the HearthDrive Android app (APK).
#
#   scripts/build-android.sh [x86_64|aarch64] [debug|release]
#
# x86_64 is for the emulator, aarch64 for real phones. Run it from Git Bash on
# Windows, or any shell on Linux/macOS. It needs the Android SDK and NDK, a
# JDK (Android Studio's is used if JAVA_HOME is not set), and the Rust target
# (`rustup target add aarch64-linux-android x86_64-linux-android`).
#
# The Tauri CLI links the compiled library into the Gradle project with a
# symbolic link, which Windows only allows in Developer Mode. This script
# copies the library instead, so it works without changing that setting.
# Release builds are signed with the keystore in NEXUS_ANDROID_KEYSTORE
# (with NEXUS_ANDROID_KEYSTORE_PASSWORD, NEXUS_ANDROID_KEY_ALIAS and, if it
# differs, NEXUS_ANDROID_KEY_PASSWORD), or else with the local Android debug
# key so they can be installed directly. Updates install over the app only
# when they are signed with the same key as the installed one.
set -euo pipefail

ARCH="${1:-aarch64}"
PROFILE="${2:-debug}"
# On Git Bash, `pwd -W` gives a Windows path (D:/...), which both bash and
# the Windows NDK tools understand, with or without MSYS path conversion.
ROOT="$(cd "$(dirname "$0")/.." && { pwd -W 2>/dev/null || pwd; })"
APP="$ROOT/apps/desktop"
GRADLE="$APP/src-tauri/gen/android"

case "$ARCH" in
  x86_64)  TRIPLE=x86_64-linux-android;  ABI=x86_64;    FLAVOR=X86_64 ;;
  aarch64) TRIPLE=aarch64-linux-android; ABI=arm64-v8a; FLAVOR=Arm64 ;;
  *) echo "unknown architecture: $ARCH (use x86_64 or aarch64)" >&2; exit 2 ;;
esac
case "$PROFILE" in
  debug)   VARIANT=Debug;   CLI_FLAG=--debug ;;
  release) VARIANT=Release; CLI_FLAG= ;;
  *) echo "unknown profile: $PROFILE (use debug or release)" >&2; exit 2 ;;
esac

: "${ANDROID_HOME:=${LOCALAPPDATA:-$HOME}/Android/Sdk}"
if [ -z "${NDK_HOME:-}" ]; then
  NDK_HOME="$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -1)"
fi
if [ -z "${JAVA_HOME:-}" ] && [ -d "/c/Program Files/Android/Android Studio/jbr" ]; then
  JAVA_HOME="C:/Program Files/Android/Android Studio/jbr"
fi
export ANDROID_HOME NDK_HOME JAVA_HOME
[ -d "$NDK_HOME" ] || { echo "Android NDK not found under $ANDROID_HOME/ndk" >&2; exit 1; }

HOST_TAG="$(ls "$NDK_HOME/toolchains/llvm/prebuilt" | head -1)"
STRIP="$NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG/bin/llvm-strip"
LIB="$ROOT/target/$TRIPLE/$PROFILE/libnexus_app.so"
JNI="$GRADLE/app/src/main/jniLibs/$ABI"

cd "$APP"
echo "==> Compiling the app library ($TRIPLE, $PROFILE)"
# The CLI builds the web UI and the Rust library, then tries to link and
# package. Where symbolic links are not allowed that last part fails, and the
# steps below finish the job.
if npx tauri android build $CLI_FLAG --target "$ARCH" --apk; then
  LINKED=1
else
  LINKED=0
  [ -f "$LIB" ] || { echo "The Rust build failed." >&2; exit 1; }
  # Make sure the library is newer than the sources we just built from.
  if [ -n "$(find "$APP/dist/index.html" -newer "$LIB" 2>/dev/null)" ]; then
    echo "The Rust build failed (library is older than the web UI)." >&2; exit 1
  fi
fi

if [ "$LINKED" = 0 ]; then
  echo "==> Copying the library into the Gradle project"
  mkdir -p "$JNI"
  rm -f "$JNI/libnexus_app.so"
  "$STRIP" --strip-debug -o "$JNI/libnexus_app.so" "$LIB"
  echo "==> Packaging with Gradle"
  # Start from a fresh APK: repacking in place leaves the old library's
  # bytes behind as dead space and doubles the file size.
  rm -rf "$GRADLE/app/build/outputs/apk"
  (cd "$GRADLE" && ./gradlew --console=plain "assemble$FLAVOR$VARIANT" -x "rustBuild$FLAVOR$VARIANT")
fi

OUT_DIR="$GRADLE/app/build/outputs/apk/$(echo "$FLAVOR" | tr '[:upper:]' '[:lower:]')/$PROFILE"
[ -d "$OUT_DIR" ] || OUT_DIR="$(dirname "$(find "$GRADLE/app/build/outputs/apk" -name '*.apk' -newer "$LIB" | head -1)")"
APK="$(ls "$OUT_DIR"/*.apk | head -1)"

if [ "$PROFILE" = release ] && [[ "$APK" == *unsigned* ]]; then
  BT="$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -1)"
  if [ -n "${NEXUS_ANDROID_KEYSTORE:-}" ]; then
    echo "==> Signing with $NEXUS_ANDROID_KEYSTORE"
    KEYSTORE="$NEXUS_ANDROID_KEYSTORE"
    : "${NEXUS_ANDROID_KEYSTORE_PASSWORD:?NEXUS_ANDROID_KEYSTORE_PASSWORD is needed with NEXUS_ANDROID_KEYSTORE}"
    : "${NEXUS_ANDROID_KEY_ALIAS:?NEXUS_ANDROID_KEY_ALIAS is needed with NEXUS_ANDROID_KEYSTORE}"
    export NEXUS_ANDROID_KEY_PASSWORD="${NEXUS_ANDROID_KEY_PASSWORD:-$NEXUS_ANDROID_KEYSTORE_PASSWORD}"
    export NEXUS_ANDROID_KEYSTORE_PASSWORD
    ALIAS="$NEXUS_ANDROID_KEY_ALIAS"
    PASSES=(--ks-pass env:NEXUS_ANDROID_KEYSTORE_PASSWORD --key-pass env:NEXUS_ANDROID_KEY_PASSWORD)
  else
    echo "==> Signing with the local debug key"
    KEYSTORE="${USERPROFILE:-$HOME}/.android/debug.keystore"
    if [ ! -f "$KEYSTORE" ]; then
      mkdir -p "$(dirname "$KEYSTORE")"
      "$JAVA_HOME/bin/keytool" -genkeypair -keystore "$KEYSTORE" -storepass android -keypass android \
        -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Android Debug,O=Android,C=US"
    fi
    ALIAS=androiddebugkey
    PASSES=(--ks-pass pass:android --key-pass pass:android)
  fi
  SIGNED="${APK/-unsigned/}"
  "$BT/zipalign" -f -p 4 "$APK" "$SIGNED.aligned"
  APKSIGNER="$BT/apksigner"; [ -f "$APKSIGNER.bat" ] && APKSIGNER="$APKSIGNER.bat"
  "$APKSIGNER" sign --ks "$KEYSTORE" "${PASSES[@]}" --ks-key-alias "$ALIAS" --out "$SIGNED" "$SIGNED.aligned"
  rm -f "$SIGNED.aligned"
  APK="$SIGNED"
fi

echo
echo "APK: $APK"
echo "Install on a connected phone with:"
echo "  \"$ANDROID_HOME/platform-tools/adb\" install -r \"$APK\""
