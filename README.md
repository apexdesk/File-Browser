# File Browser

A fast, lightweight, cross-platform file manager written in Rust with
[egui](https://github.com/emilk/egui). It runs on Linux, Windows, macOS and Android and is designed to
stay responsive in folders with hundreds of thousands of entries.

The crate, the binary and the library are called `filebrowser`. The interface is available in 10 languages
(English, Spanish, French, German, Portuguese, Italian, Russian, Chinese, Japanese, Korean) and follows
the system language by default.

## Build and run

```sh
cargo run --release [-- /initial/path]
```

### Windows (cross-compiled from Linux)

```sh
cargo install cargo-xwin
RUSTFLAGS="-C target-feature=+crt-static" \
  cargo xwin build --release --target x86_64-pc-windows-msvc
```

The result is a single self-contained `.exe`.

### Android

Requires the Android SDK (build-tools 34, platform 34), the NDK and the `aarch64-linux-android` Rust
target. Then:

```sh
./android/build-apk.sh        # produces target-android/FileBrowser.apk (signed with the debug key)
```

`ANDROID_HOME` and `NDK` can be set to point at your SDK/NDK.

## Releases and CI

GitHub Actions (`.github/workflows/ci.yml`) runs the tests and builds every target on each push and pull
request: Linux x86-64 and aarch64, Windows x86-64 and ARM64, macOS (universal `.app`) and Android arm64.
Pushing a tag such as `v0.2.0` publishes a GitHub release with all binaries and `SHA256SUMS`.

To sign the Android APK with your own key instead of a throw-away debug key, add the repository secrets
`ANDROID_KEYSTORE_B64` (base64 of the keystore), `ANDROID_KS_PASS` and `ANDROID_KS_ALIAS`.

## Performance design

- **Streaming directory listing** on a worker thread: the first chunk is painted in ~1 ms; 300,000
  entries load in ~0.25 s.
- **Sorting and filtering work on `u32` indices** on a separate thread, so the UI never blocks.
- **Virtualised grid/list**: only visible rows are drawn (plus one row of prefetch).
- **Thumbnails**: a "visible first" queue that is replaced every frame (scrolling fast drops stale
  work), a thread pool, JPEG decoding with DCT downscaling (1/2–1/8) and an LRU texture cache.
- **Vector icons** drawn with the painter: no image or icon fonts needed.
- **No continuous repainting**: 0 % CPU at idle.

## Features

- **Views**: icon grid and list. In the list, columns are configurable in *Options → Columns*
  (modified, size, type, extension, permissions, owner, group, item count, created, last access,
  octal mode, link target); they can be reordered and hidden columns drop out first on narrow windows.
- **Tabs** and a **split view** (two panes, side by side or stacked), with the full view saved per tab
  and restored at startup.
- **Menu bar / hamburger menu**, **command palette** (`Shift+Space`) and **remappable shortcuts**.
- **File operations**: copy, move, rename (including batch rename), duplicate, new file/folder, trash
  (browsable on Linux and Windows) and permanent delete, **undo/redo**, an operation queue with
  progress, speed, ETA and cancel, and a conflict dialog (replace / keep both / skip; replaced items go
  to the trash).
- **Properties** with recursive size computed in the background; name, modification time and
  permissions can be edited in place.
- **Tools**: ZIP compress/extract, hashes (MD5, SHA-1, SHA-256), duplicate finder, search in
  subfolders, "Open with…".
- **Quick preview** (`Space`) for images, text and video (video needs `ffmpeg`/`ffplay`), optionally in
  a separate window. Extra thumbnail formats (SVG, HEIC, video…) via `ffmpeg` or ImageMagick when
  installed.
- **Integrated terminal** (`F12`) with a real PTY and VT100 emulation (Linux/macOS).
- **Network locations** (Linux): connect to SMB/SFTP/FTP/WebDAV servers through gvfs, choose which
  share to mount, and unmount from the sidebar.
- **System icon theme** (freedesktop on Linux, Shell on Windows) or built-in vector icons.
- **Light/dark theme** that follows the system, with a smooth transition when switching.
- **Android**: touch-first UI with safe-area handling, a phone layout, long-press context menus,
  corner-tap selection and single-tap open (configurable on every platform).
- **Hidden files**: toggle them, and names listed in a folder's `.hidden` file are hidden too.

## Keyboard shortcuts

| Key | Action |
|---|---|
| `Space` | Quick preview (←/→ navigate, `Esc`/`Space` close, `Enter` opens) |
| `Enter` / double click | Open |
| `Backspace`, `Alt+↑` | Parent folder · `Alt+←/→` back/forward |
| `F2` | Rename · `Del` trash · `Shift+Del` delete permanently |
| `Ctrl+C/X/V` | Copy/cut/paste · `Ctrl+A` select all |
| `Ctrl+L` | Edit path · `Ctrl+F` filter · `Ctrl+D` bookmark |
| `Ctrl+H` | Hidden files · `Ctrl+Shift+N` new folder · `F5` refresh |
| `Ctrl+B` | Show/hide the sidebar |
| `Ctrl+T` / `Ctrl+W` | New / close tab · `Ctrl+Tab` switch · `Ctrl+Shift+T` reopen |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo |
| `F9` | Split view · `Tab` switch pane · `F6`/`F7` copy/move to the other pane |
| `F12` | Terminal |
| Type a name | Jump to the first entry starting with it |

Drag items onto a folder or onto the sidebar to move them; drag on empty space for a rubber-band
selection. Dropping files from another application onto the window copies them to the current folder.

## System integration

- **Command line**: `filebrowser [-n|--new-window] [PATH...]`. Folders open as tabs; a file opens its folder with the file selected. A second launch forwards its paths to the running instance (use `-n` to force a new window).
- **Default file manager**: Options → *Use as system file manager*, or `filebrowser --install` / `--uninstall`. Linux registers a `.desktop` entry and `inode/directory`; Windows adds *Open with File Browser* to the folder and folder-background context menus; macOS declares folder document types in the app bundle's `Info.plist`.
- **Drag and drop**: selected items can be dragged out to other applications (Wayland, X11, Windows, macOS) and files dropped from other apps are copied into the current folder. Not available on mobile.
- **Running files**: double-clicking a program runs it directly; *Open with* is a submenu listing installed applications.

## Known limitations

- Dragging files **out** of the application into other apps is not supported on Linux (winit does not
  provide a drag source). Use copy and paste instead.
- On Windows and macOS, the file clipboard shared with other apps is not implemented (only paths as
  text). On Linux it needs `wl-copy`/`xclip`.
- Network locations rely on gvfs and are Linux-only.
- The Windows and macOS builds have received much less testing than Linux and Android.

## License

MIT — see [LICENSE](LICENSE).
