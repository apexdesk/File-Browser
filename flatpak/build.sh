#!/bin/sh
# Empaqueta el binario de Linux x86_64 ya compilado como bundle Flatpak (sin flatpak-builder).
# Uso: flatpak/build.sh <filebrowser-linux-x86_64.tar.gz> <versión> <salida.flatpak>
# Requiere: flatpak y org.freedesktop.Platform/Sdk 25.08 instalados.
set -e
ID=io.github.apexdesk.FileBrowser
TAR=$(realpath "$1"); VER=$2; OUT=$(realpath -m "$3")
HERE=$(cd "$(dirname "$0")" && pwd)
W=$(mktemp -d); trap 'rm -rf "$W"' EXIT
tar xzf "$TAR" -C "$W"
cd "$W"
flatpak build-init build $ID org.freedesktop.Sdk org.freedesktop.Platform 25.08
F=build/files
mkdir -p $F/bin $F/share/applications $F/share/metainfo $F/share/icons/hicolor/256x256/apps $F/share/icons/hicolor/scalable/apps
install -m755 filebrowser $F/bin/filebrowser
cp "$HERE/../assets/icon-256.png" $F/share/icons/hicolor/256x256/apps/$ID.png
cp "$HERE/../assets/icon.svg" $F/share/icons/hicolor/scalable/apps/$ID.svg
cp "$HERE/$ID.desktop" $F/share/applications/
sed "s/@VERSION@/$VER/; s/@DATE@/$(date -u +%F)/" "$HERE/$ID.metainfo.xml" > $F/share/metainfo/$ID.metainfo.xml
flatpak build-finish build --command=filebrowser --share=ipc --socket=wayland --socket=fallback-x11 \
  --device=dri --socket=pulseaudio --filesystem=host --own-name=org.freedesktop.FileManager1 --talk-name=org.freedesktop.Notifications \
  '--talk-name=org.gtk.vfs.*' --filesystem=xdg-run/gvfs --filesystem=xdg-run/gvfsd
flatpak build-export repo build
flatpak build-bundle repo "$OUT" $ID
