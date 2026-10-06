# Roneyview (version 0.5.0)

An alternative to Honeyview—an image viewer for Linux—written in the Rust programming language (using egui/eframe).

## What can Roneyview do?
- **Open images**, **folders**, and **archives**
- **Supports image formats**: JPEG/JPG, PNG, GIF, WebP, BMP, TIFF, QOI, TGA, ICO, AVIF, HEIC/HEIF
- **Supports animated image formats**: GIF, WebP, and APNG
- **Press the 'P' key to pause/play animated image files**
- **Supports archive formats**: ZIP/CBZ/RAR/CBR; displays only the image files contained within the archive
- **Does not save viewing history**: no list of images or positions from previous sessions; if you launch Roneyview from the command line without specifying a file or folder path, it will simply open an empty window
- **Right-clicking on an image displays the following menu**:
- **Set as wallpaper...**
- **Properties**
- **Actions**:
- Open containing folder
- Move to trash
- **Two-page mode** and **right-to-left reading direction** for manga/comics
- Window-fit, width-fit, height-fit, and 100% view modes; zoom to cursor, pan, rotate, and full-screen
- **Previous/Next buttons on the left/right edges**; appears only when the pointer approaches the edge
- **Bottom bar**:
- **prev/next buttons**
- **seek slider**
- **'lock' checkbox to keep the 'bottom bar' visible**

## Debian/Ubuntu/MX Linux/other Debian derivatives

```bash
# 1. Build tools + libraries for file dialogs (GTK3) and AVIF/HEIC (libheif)
sudo apt update
sudo apt install build-essential pkg-config libgtk-3-dev libheif-dev curl

# 2. Latest Rust (the rustc package in Debian is too old; requires Rust >= 1.88)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 3. Compile and install to ~/.local (without sudo)
./install.sh

All dependencies required for the smooth operation of roneyview will be downloaded automatically when you run ./install.sh.
However, if you wish to see which dependencies Roneyview requires, you can view them [here](supporting-roneyview.md)
```

`install.sh` checks for available packages and enables only the features that can be built,
then displays which features were skipped along with the `apt` commands to install them.
`./install.sh --dry-run` simply displays these options without compiling.

| Feature | Build-time requirement | Notes |
|---|---|---|
| `dialogs` (Open file dialog) | `libgtk-3-dev` | drag-and-drop and command-line arguments still work without this |
| `heif` (AVIF, HEIC/HEIF) | `libheif-dev` **>= 1.17** | see version notes below |
| `rar` (RAR/CBR) | `g++` (from `build-essential`) | unrar code automatically compiled by Cargo |

Once installed, `roneyview` is available in the terminal (ensure `~/.local/bin` is in your `PATH`)
and appears in the application menu as well as the "Open with" option in file managers.
Remove it using `./install.sh --uninstall`.

### libheif version notes (AVIF/HEIC)

- **MX Linux 25 / Debian 13**: libheif 1.19; supported. If HEIC or AVIF files fail to open with
a "decoder" error, install the codec plugins: `sudo apt install libheif-plugin-libde265 libheif-plugin-dav1d`.
- **MX Linux 23 / Debian 12**: libheif is only version 1.15, which is **too old** for the crate used
(requires >= 1.17). `install.sh` automatically skips this feature; all other features remain functional.
- Build manually without specific features, e.g.: `cargo build --release --locked --no-default-features --features dialogs,rar`

### RAR notes

RAR files can only be read sequentially (no random access). Roneyview remembers the read position within the archive while it is open,
so opening the next page is fast, but jumping backward reopens the archive from the beginning. With large *solid*
archives, large jumps may feel slow. Password-protected archives are not supported.

## Shortcuts

| Key | Function |
|---|---|
| Right / PgDn / Space | Next image |
| Left / PgUp / Backspace | Previous image |
| Home / End | First / last |
| Mouse wheel | Vertical scroll; switch image when at the edge |
| Up / Down, left-click drag | Pan image |
| Ctrl + wheel, `+`, `-` | Zoom |
| `F` / `Shift+F` | Fit to window (shrink only / shrink & enlarge) |
| `W` / `H` | Fit to width / height |
| `0` or `1` | Original size (100%) |
| `R` / `Shift+R` | Rotate right / left |
| `P` | Pause / play animation |
| `L` | Lock / unlock bottom bar |
| `M` | Memory-saving mode |
| Enter, F11, double-click | Full screen (Esc to exit) |
| Ctrl+O / Ctrl+Shift+O | Open file or archive / open folder |
| F1 | Shortcut list |
| Ctrl+Q | Quit |

## Command-line usage

```bash
roneyview photo.jpg        # open photo; the entire folder is also loaded
roneyview ~/Comic_title/Chapter1    # open folder
roneyview title_comic.cbz       # open archive (also .zip, .rar, .cbr)
roneyview                 # empty window; Ctrl+O to open an image
```

Only **display options** are saved in `~/.config/roneyview/state.json` (zoom mode, bar lock, memory-saving
mode, mo...two-page layout, reading direction, and the final option in the wallpaper window). Old files (version <= 0.4)
containing position history will be rewritten without that history when Roneyview is launched.

```bash
roneyview --set-wallpaper photo.jpg --style zoomed --color '#202020'   # without opening a window
roneyview --restore-wallpaper                                          # restore root window wallpaper
```

## Memory limits

Roneyview limits its own memory usage to prevent the system from running out of RAM:

- Texture size per page is limited by **longest side** (GPU limit) and **pixel count** (max. 4 to 16 MP,
adjusted based on available RAM at startup). 50 MP photos are displayed using a downscaled version.
- The page cache has a total budget of 96 to 256 MB (including mipmaps); the most distant pages are discarded first.
- Small files that **claim** to have a massive size (decompression bombs) are rejected before decoding, with a clear error message.
- If available system memory drops below 256 MB, prefetching is disabled and the cache is cleared.

### Memory-saving mode (`M` key or View menu)

Only the image **currently being viewed** is stored in RAM; previous images are discarded as soon as you move away,
and there is no prefetching. This setting persists across sessions. The trade-off: page navigation feels slower for
large photos because each image must be decoded upon opening (including when returning to a previous image).
In a test using twenty-four 24 MP photos, stable RAM usage dropped from approximately 395 MB to 232 MB.

### What Roneyview does not do

- Roneyview does not use `tumblerd` or any system thumbnailer, nor does it generate thumbnails; Images are decoded on the fly
- No image caching on disk and no history of opened images. The only files written are
`~/.config/roneyview/state.json` (display preferences) and, if you use *Set as wallpaper*, a copy of the
wallpaper image in `~/.local/share/roneyview/`. When Roneyview closes, all its memory is released by the system.

## Right-click menu

Right-click on an image (in two-page mode: on the clicked page):

- **Set as wallpaper...** opens a new window with a live preview,
**Color** (**Solid color** with a color picker + hex field next to it, or **Transparent**) and **Style**
(*None, Centered, Tiled, Stretched, Scaled, Zoomed*), followed by an **Apply** button. Images from ZIP/RAR or formats
unrecognized by the desktop (HEIC, AVIF, WebP, ...) are automatically exported to a temporary PNG.
- **Properties** displays the filename, location, image dimensions, file size, format, and modification date. For
images inside archives: the archive path and the path within the archive.
- **Actions**: *Open in folder* (opens the file manager and selects the file) and *Move to trash*
(with confirmation; follows the freedesktop.org Trash specification). If the image is inside an archive, both
options simply display a warning, and Roneyview performs no further action.

### Set as wallpaper across different desktops

There is no single method that works on all desktops, as desktop environments manage their backgrounds
independently. Roneyview detects the running desktop and uses its official mechanism:

| Desktop | Method | Test status |
|---|---|---|
| Xfce | `xfconf-query` (xfce4-desktop property) | tested with actual xfdesktop 4.18 |
| Window managers without a desktop manager (Fluxbox, Openbox, i3, ...) | draws directly to the X11 root window (all styles, multi-monitor support via RandR) + autostart `roneyview --restore-wallpaper` | tested in Xvfb |
| GNOME, Budgie, Unity | `gsettings` (`org.gnome.desktop.background`) | keys and values ​​validated against the original schema |
| Cinnamon | `gsettings` (`org.cinnamon.desktop.background`) | validated against the original schema |
| MATE | `gsettings` (`org.mate.background`) | validated against the original schema |
| KDE Plasma | Plasma script via D-Bus (`gdbus`) | **not yet tested on Plasma**; only the command line was checked |
| LXDE / LXQt | `pcmanfm --set-wallpaper` / `pcmanfm-qt` | **not yet tested**; only the command line was checked |
| Sway | `swaymsg output * bg` | **not yet tested**; only the command line was checked |
| Other Wayland environments, GNOME/KDE without the commands above | not supported | clear message displayed in the window |

Note: *Transparent* is only meaningful in Xfce; black is used on other desktops.

## Two-page mode and reading direction

- `D` toggles **two-page mode**; `K` switches the **reading direction** between left-to-right and right-to-left (manga).
- The View menu also includes **Single first page (cover)**: the first page is displayed alone, followed by pairs
2-3, 4-5, ...
- **Wide** pages (landscape, e.g., double-page scans) are always displayed individually; the height of the two pages is matched. - In right-to-left mode, the first page is on the right; **left arrow = next**, and side buttons, bars,
and sliders are mirrored accordingly. `Space`/`PgDn` still means "next."
- Rotation (`R`) is not available in two-page mode.

## Animation limitations

All animation frames are stored in memory. (max. approx. 128 MB per animation, 1500 frames).
Larger animations are truncated, and the status bar displays "truncated".

## Development

```bash
cargo test            # unit tests: natural order, ZIP/RAR, animated GIF, HEIC/AVIF, memory budget, slider, zoom,
# wallpaper layout, desktop detection, gsettings/KDE/pcmanfm/sway commands, dual-page view
cargo run -- /path/to/image
```
