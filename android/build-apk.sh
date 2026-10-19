#!/usr/bin/env bash
# Empaqueta libfilebrowser.so en un APK firmado. Uso: build-apk.sh [arm64-v8a|armeabi-v7a] (por defecto arm64-v8a).
# Sin ANDROID_KEYSTORE se firma con la clave de depuración.
set -euo pipefail
cd "$(dirname "$0")/.."
SDK=${ANDROID_HOME:-$HOME/android-sdk}
BT=$SDK/build-tools/34.0.0
JAR=$SDK/platforms/android-34/android.jar
NDK=${NDK:-$SDK/ndk/27.3.13750724}
export PATH=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin:$PATH
ABI=${1:-arm64-v8a}
case "$ABI" in
  arm64-v8a)   TRIPLE=aarch64-linux-android;   CLANG=aarch64-linux-android26-clang;   OUTAPK=FileBrowser.apk ;;
  armeabi-v7a) TRIPLE=armv7-linux-androideabi; CLANG=armv7a-linux-androideabi26-clang; OUTAPK=FileBrowser-armv7.apk ;;
  *) echo "ABI desconocida: $ABI" >&2; exit 1 ;;
esac
ENVT=${TRIPLE//-/_}
UPT=$(echo "$ENVT" | tr a-z A-Z)
export CARGO_TARGET_DIR=target-android "CC_$ENVT=$CLANG" "AR_$ENVT=llvm-ar" "CARGO_TARGET_${UPT}_LINKER=$CLANG"
cargo build --release --lib --target "$TRIPLE"
OUT=target-android/apk-$ABI
rm -rf "$OUT" && mkdir -p "$OUT/lib/$ABI"
cp "target-android/$TRIPLE/release/libfilebrowser.so" "$OUT/lib/$ABI/"
mkdir -p "$OUT/res/mipmap-xxxhdpi" "$OUT/compiled"
cp assets/icon-512.png "$OUT/res/mipmap-xxxhdpi/ic_launcher.png"
"$BT/aapt2" compile --dir "$OUT/res" -o "$OUT/compiled"
"$BT/aapt2" link -o "$OUT/base.apk" -I "$JAR" --manifest android/AndroidManifest.xml "$OUT"/compiled/*.flat
(cd "$OUT" && zip -q -r base.apk lib)
"$BT/zipalign" -f -p 4 "$OUT/base.apk" "$OUT/aligned.apk"
KS=${ANDROID_KEYSTORE:-$HOME/.android/debug.keystore}
KS_PASS=${ANDROID_KS_PASS:-android}
KS_ALIAS=${ANDROID_KS_ALIAS:-androiddebugkey}
if [ ! -f "$KS" ]; then
  mkdir -p "$HOME/.android"
  keytool -genkeypair -keystore "$KS" -storepass android -keypass android -alias androiddebugkey \
    -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Android Debug,O=Android,C=US"
fi
"$BT/apksigner" sign --ks "$KS" --ks-pass "pass:$KS_PASS" --key-pass "pass:$KS_PASS" --ks-key-alias "$KS_ALIAS" \
  --out "target-android/$OUTAPK" "$OUT/aligned.apk"
echo "APK: $(pwd)/target-android/$OUTAPK"
