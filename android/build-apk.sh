#!/usr/bin/env bash
# Empaqueta libfilebrowser.so en un APK firmado. Uso: build-apk.sh [arm64-v8a|armeabi-v7a] (por defecto arm64-v8a).
# Sin ANDROID_KEYSTORE se firma con la clave de depuración (con un aviso: no sirve para publicar).
# Con ANDROID_KEYSTORE hacen falta ANDROID_KS_ALIAS y ANDROID_KS_PASS o ANDROID_KS_PASS_FILE.
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
# Firma. La contraseña nunca va en la línea de órdenes (la vería cualquiera con `ps`): apksigner la
# lee de la variable ANDROID_KS_PASS (env:) o del archivo ANDROID_KS_PASS_FILE (file:).
if [ -n "${ANDROID_KEYSTORE:-}" ]; then
  KS=$ANDROID_KEYSTORE
  KS_ALIAS=${ANDROID_KS_ALIAS:?ANDROID_KS_ALIAS es obligatorio con ANDROID_KEYSTORE}
  if [ -n "${ANDROID_KS_PASS_FILE:-}" ]; then
    PASS_SRC="file:$ANDROID_KS_PASS_FILE"
  else
    : "${ANDROID_KS_PASS:?falta ANDROID_KS_PASS o ANDROID_KS_PASS_FILE}"
    export ANDROID_KS_PASS
    PASS_SRC="env:ANDROID_KS_PASS"
  fi
else
  KS=$HOME/.android/debug.keystore
  KS_ALIAS=androiddebugkey
  export FB_DEBUG_KS_PASS=android
  PASS_SRC="env:FB_DEBUG_KS_PASS"
  if [ ! -f "$KS" ]; then
    mkdir -p "$HOME/.android"
    keytool -genkeypair -keystore "$KS" -storepass:env FB_DEBUG_KS_PASS -keypass:env FB_DEBUG_KS_PASS -alias androiddebugkey \
      -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Android Debug,O=Android,C=US"
  fi
  {
    echo "################################################################################"
    echo "## AVISO: APK firmado con la CLAVE DE DEPURACIÓN ($KS)."
    echo "## No sirve para publicar: cualquiera puede generar una igual, y un APK firmado"
    echo "## después con la clave de verdad no podrá actualizar a éste (ni al revés)."
    echo "## Para una versión: ANDROID_KEYSTORE, ANDROID_KS_ALIAS y ANDROID_KS_PASS(_FILE)."
    echo "################################################################################"
  } >&2
fi
"$BT/apksigner" sign --ks "$KS" --ks-pass "$PASS_SRC" --key-pass "$PASS_SRC" --ks-key-alias "$KS_ALIAS" \
  --out "target-android/$OUTAPK" "$OUT/aligned.apk"
echo "APK: $(pwd)/target-android/$OUTAPK"
