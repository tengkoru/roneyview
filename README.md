# Roneyview

A fast image viewer alternative to Honeyview for Linux, written in Rust (egui/eframe).

- Open a single image (its parent folder is loaded automatically), a folder, or an archive **ZIP/CBZ/RAR/CBR**
- Formats: JPEG, PNG, GIF, WebP, BMP, TIFF, QOI, TGA, ICO, AVIF, HEIC/HEIF
- Animation: GIF, WebP, and APNG (`P` toggles pause/play)
- Archive types are detected from file contents, not extension (a `.cbz` that is actually a RAR still opens)
- Natural filename ordering (`p2` before `p10`), EXIF orientation respected
- Prefetch nearby pages in a separate thread; the UI never waits for the decoder
- **Resume from the last position** for each folder/archive
- Window load / width / height / 100% modes, zoom toward cursor, drag, rotate, fullscreen
- CPU-efficient idle (no redraw when nothing changed)
- **Previous/Next buttons on the left/right edges** that appear only when the pointer nears the edge
- **Bottom bar**: Previous/Next buttons, a **jump slider** to any image (e.g. 46/100) with
  filename hints while dragging, and a **Lock** checkbox to keep the bar visible
- Memory usage is capped according to available RAM (see "Memory limits")

## Installing on MX Linux or other Debian-based systems

```bash
# 1. Build tools + libraries for file dialogs (GTK3) and AVIF/HEIC (libheif)
sudo apt update
sudo apt install build-essential pkg-config libgtk-3-dev libheif-dev curl

# 2. Latest Rust (the Debian package is too old; Rust >= 1.88 is required)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 3. Build and install into ~/.local (no sudo)
./install.sh
```

`install.sh` checks which packages are available and only enables features that can be built,
then shows which features are skipped and the `apt` commands needed to install them.
`./install.sh --dry-run` only shows that selection without building.

| Feature | Needed at build time | Notes |
|---|---|---|
| `dialogs` (Open file dialogs) | `libgtk-3-dev` | without it, drag-and-drop and command-line arguments still work |
| `heif` (AVIF, HEIC/HEIF) | `libheif-dev` **>= 1.17** | see version notes below |
| `rar` (RAR/CBR) | `g++` (from `build-essential`) | unrar code is automatically compiled by Cargo |

After installation, `roneyview` is available in the terminal (make sure `~/.local/bin` is in `PATH`)
and appears in the app menu and "Open with" in the file manager.
Remove it with `./install.sh --uninstall`.

### libheif version notes (AVIF/HEIC)

- **MX Linux 25 / Debian 13**: libheif 1.19, supported. If HEIC or AVIF fails to open with a
  "decoder" message, install the codec plugins: `sudo apt install libheif-plugin-libde265 libheif-plugin-dav1d`.
- **MX Linux 23 / Debian 12**: libheif is only 1.15, **too old** for the crate in use
  (needs >= 1.17). `install.sh` automatically skips this feature; all other features continue to work.
- Built manually without a specific feature, for example: `cargo build --release --locked --no-default-features --features dialogs,rar`

### RAR notes

RAR can only be read sequentially (not random access). Roneyview stores the read position, so moving to the
next page is fast, but jumping backward reopens the archive from the beginning. In large *solid* archives,
long jumps may feel slow. Password-protected archives are not supported.

## Shortcuts

| Key | Function |
|---|---|
| Right / PgDn / Space | Next image |
| Left / PgUp / Backspace | Previous image |
| Home / End | First / last |
| Mouse wheel | Vertical scroll; move to the next image when at the limit |
| Up / Down, left drag | Pan image |
| Ctrl + wheel, `+`, `-` | Zoom |
| `F` / `Shift+F` | Fit to window (shrink only / enlarge too) |
| `W` / `H` | Fit width / height |
| `0` or `1` | Actual size 100% |
| `R` / `Shift+R` | Rotate right / left |
| `P` | Pause / play animation |
| `L` | Lock / unlock bottom bar |
| Enter, F11, double click | Fullscreen (Esc exits) |
| Ctrl+O / Ctrl+Shift+O | Open file or archive / open folder |
| F1 | Shortcut list |
| Ctrl+Q | Exit |

## Set Roneyview as the default for some formats
``` bash
xdg-mime default roneyview.desktop image/jpg image/png image/jpeg image/heic image/heif image/gif image/webp image/avif application/zip application/vnd.comicbook+zip application/vnd.rar
```
## Command-line usage

```bash
roneyview photo1.jpg        # open a photo; the whole folder is loaded with it
roneyview ~/Comic_Title/Chap1    # open a folder
roneyview Comic_Title.cbz       # open an archive (also .zip, .rar, .cbr)
roneyview                 # continue from the last position
```

Settings and the last position are saved in `~/.config/roneyview/state.json`.

## Side buttons and bottom bar

- Move the pointer to the **left/right edge** of the window: arrow buttons appear. The previous button does not appear
  on the first image, and the next button does not appear on the last image.
- Move the pointer to the **bottom**: a bar appears containing buttons, a slider, a `46/100` indicator, and a **Lock** checkbox.
- **Slider**: hold and drag; the new page opens when the mouse button is **released** (not on every pixel moved), so dragging across hundreds of large photos does not load them all. Clicking the slider jumps immediately.
  While held, the bar does not disappear even if the pointer leaves the bar area.
- **Lock** (or the `L` key): keep the bar visible even when the pointer moves away. This option is remembered between sessions.
- All of these elements work in fullscreen mode as well.

## Memory limits

Roneyview limits its own memory so it does not drag the system down to RAM exhaustion:

- Texture size per page is capped by the **longest side** (GPU limit) and by the **pixel count** (max. 4 to 16 MP,
  adjusted based on available RAM at startup). A 50 MP photo is displayed at a reduced scale.
- Page cache has a total budget of 96 to 256 MB (including mipmaps); the farthest pages are discarded first.
- Small files that **claim** to be giant (decompression bomb) are rejected before decoding with a clear message.
- If available system memory drops below 256 MB, prefetch is disabled and the cache is cleared.

## Animation limits

All animation frames are kept in memory (up to about 128 MB per animation, 1500 frames).
Larger animations are cropped and the status bar shows "cropped".

## Known issues, solutions?
1. The screen goes black because tumbler uses RAM continuously after launching Roneyview, which occurs in the Thunar file manager.
Solution: Open the Thunar file manager -> Edit -> Preferences -> Show thumbnails (Never). This reduces the chance of the black screen, but does not eliminate it completely.

## Development

```bash
cargo test            # 37 unit tests (natural ordering, ZIP/RAR, GIF animation, HEIC/AVIF, memory budget, slider, zoom)
cargo run -- /path/to/image
```
