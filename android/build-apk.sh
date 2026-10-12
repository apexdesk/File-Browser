#!/usr/bin/env bash
# Empaqueta libfilebrowser.so (aarch64) en un APK firmado con la clave de depuración.
set -euo pipefail
cd "$(dirname "$0")/.."
SDK=${ANDROID_HOME:-$HOME/android-sdk}
BT=$SDK/build-tools/34.0.0
JAR=$SDK/platforms/android-34/android.jar
NDK=${NDK:-$SDK/ndk/27.3.13750724}
export PATH=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin:$PATH
CARGO_TARGET_DIR=target-android CC_aarch64_linux_android=aarch64-linux-android26-clang \
  AR_aarch64_linux_android=llvm-ar CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=aarch64-linux-android26-clang \
  cargo build --release --lib --target aarch64-linux-android
OUT=target-android/apk
rm -rf "$OUT" && mkdir -p "$OUT/lib/arm64-v8a"
cp target-android/aarch64-linux-android/release/libfilebrowser.so "$OUT/lib/arm64-v8a/"
mkdir -p "$OUT/res/mipmap-xxxhdpi" "$OUT/compiled"
cp assets/icon-512.png "$OUT/res/mipmap-xxxhdpi/ic_launcher.png"
"$BT/aapt2" compile --dir "$OUT/res" -o "$OUT/compiled"
"$BT/aapt2" link -o "$OUT/base.apk" -I "$JAR" --manifest android/AndroidManifest.xml "$OUT"/compiled/*.flat
(cd "$OUT" && zip -q -r base.apk lib)
"$BT/zipalign" -f -p 4 "$OUT/base.apk" "$OUT/aligned.apk"
KS=$HOME/.android/debug.keystore
if [ ! -f "$KS" ]; then
  mkdir -p "$HOME/.android"
  keytool -genkeypair -keystore "$KS" -storepass android -keypass android -alias androiddebugkey \
    -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Android Debug,O=Android,C=US"
fi
"$BT/apksigner" sign --ks "$KS" --ks-pass pass:android --key-pass pass:android --ks-key-alias androiddebugkey \
  --out target-android/FileBrowser.apk "$OUT/aligned.apk"
echo "APK: $(pwd)/target-android/FileBrowser.apk"
