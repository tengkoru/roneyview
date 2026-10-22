# Roneyview (version 0.8.16)
###### README.md berbahasa Indonesia ada [disini](README_ID.md)

An alternative to Honeyview; an image viewer for Linux; written in the Rust programming language (using egui/eframe).

## Where I can download Deb or Flatpak files?
You can download at Releases place (location below "about" on repository github roneyview) or you just [klik this](https://github.com/tengkoru/roneyview/releases) to go to download page, scroll and klik dropdown menu named "Assets".

## What can Roneyview do?
- **Open images**, **folders**, and **archives**
- **Supports image formats**: JPEG/JPG, PNG, GIF, WebP, BMP, TIFF, QOI, TGA, ICO, AVIF, HEIC/HEIF, HDR (.hdr)
- **Supports animated image formats**: GIF, WebP, APNG, and multi-page TIFF (one frame per page)
- **Press the 'P' key to pause/play animated image files**; `,` / `.` step one frame backward/forward (auto-pauses)
- **Supports archive formats**: ZIP/CBZ/RAR/CBR; displays only the image files contained within the archive
- **Remembers where you stopped**: when you open a folder or archive directly, Roneyview reopens it at the
page you last read (stored in `~/.config/roneyview/positions.json`, up to 200 entries). **Recent folders**
are only remembered if you enable them in Settings (off by default). If you launch Roneyview from the
command line without specifying a file or folder path, it will simply open an empty window
- **Go to page** (`Ctrl+G`) jumps to any page; the **Navigate** menu also lists recent folders (when enabled)
- **Right-clicking on an image displays the following menu**:
    - **Set as wallpaper...**
    - **Properties**
    - **Actions**:
        - Open containing folder
        - Move to trash
        - Save page as image... (saves the original bytes of the page)
        - For animations: Save this frame as image... and Export all frames... (as PNG)
- **Toolbar** (hidden by default, View menu): icon-only buttons for **Crop**, **Resize**,
**Frame navigation**, **Color picker**, **HDR tone mapper**, **Print / save as PDF**, and **Projector** —
see [Toolbar and simple editing](##toolbar-and-simple-editing) below
- **About** (Help menu): a window titled with the version, developer credit, and the libraries used
- **Two-page mode** and **right-to-left reading direction** for manga/comics
- **View mode setting** : Window-fit, width-fit, height-fit, and 100% view modes; zoom to cursor, pan, rotate, and full-screen
- **Previous/Next buttons on the left/right edges**; appears only when the pointer approaches the edge
- **Bottom bar**:
    - **prev/next buttons**
    - **seek slider**
    - **'lock' checkbox to keep the 'bottom bar' visible**
    
## Debian/Ubuntu/MX Linux/other Debian derivatives

#### 1. Build tools + libraries for file dialogs (GTK3) and AVIF/HEIC (libheif)
```bash
sudo apt update
sudo apt install build-essential pkg-config libgtk-3-dev libheif-dev curl
```
#### 2. Latest Rust (the rustc package in Debian is too old; requires Rust >= 1.88)
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```
#### 3. Compile and install to ~/.local (without sudo)
```bash
./install.sh
```
All dependencies required for the smooth operation of roneyview will be downloaded automatically when you run ./install.sh.
However, if you wish to see which dependencies Roneyview requires, you can view them at Cargo.lock.

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
archives, large jumps may feel slow. **Password-protected archives are not supported.**

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
| `,` / `.` | Previous / next animation frame (auto-pauses) |
| `Ctrl+G` | Go to page... |
| `Ctrl+,` | Settings |
| `T` | Toolbar hide/unhide [default: **unhide**]|
| `L` | Lock / unlock bottom bar |
| `M` | Memory-saving mode |
| Enter, F11, double-click | Full screen (Esc to exit) |
| Ctrl+O / Ctrl+Shift+O | Open file or archive / open folder |
| F1 | Shortcut list |
| Ctrl+Q | Quit |

## Command-line usage
Are you prefer using command-line instead double-klik file on file manager? don't worry, we give that option here:
```bash
roneyview photo.jpg        # open photo; the entire folder is also loaded
roneyview ~/Comic_title/Chapter1    # open folder
roneyview title_comic.cbz       # open archive (also .zip, .rar, .cbr)
roneyview                 # empty window; Ctrl+O to open an image
roneyview --version		#  See roneyview version you use now
```

### Set wallpaper using roneyview command-line
```bash
roneyview --set-wallpaper photo.jpg --style zoomed --color '#202020'   # without opening a window
roneyview --restore-wallpaper                                          # restore root window wallpaper
```

All setting are saved in `~/.config/roneyview/state.json` (zoom mode, bar lock, memory-saving
mode, two-page layout, reading direction, language, toolbar visibility, and the final option in the wallpaper
window; recent folders only if enabled in Settings). The **last-read position** of each folder/archive is saved
separately in `~/.config/roneyview/positions.json` (up to 200 entries) and restored when you open that
folder/archive directly.

## Memory-saving mode (`M` key or via menu view)

**Only the image currently being viewed** is stored in RAM; previous images are discarded as soon as you move away,
and there is no prefetching.

## Right-click menu

Right-click on an image (in two-page mode: on the clicked page):

- **Set as wallpaper...** opens a new window with a live preview,
**Color** (**Solid color** with a color picker + hex field next to it, or **Transparent**) and **Style**
(*None, Centered, Tiled, Stretched, Scaled, Zoomed*), followed by an **Apply** button. Images from ZIP/RAR or formats
unrecognized by the desktop (HEIC, AVIF, WebP, ...) are automatically exported to a temporary PNG.
- **Properties** displays the filename, location, image dimensions, file size, format, and modification date. For
images inside archives: the archive path and the path within the archive.
- **Actions**: 
	- **Open in folder** (opens the file manager and selects the file)
	- **Move to trash**
(with confirmation; follows the freedesktop.org Trash specification). If the image is inside an archive, both
options simply display a warning, and Roneyview performs no further action.

### Set as wallpaper across different desktops

There is no single method that works on all desktop environment, as desktop environments manage their backgrounds
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

## Simple editing tool on Toolbar (T)

The toolbar sits below the main menu and is hidden by default; show it from the View menu
(or hide it again with the X button at its right end). Its buttons are icon-only:

- **Crop image**: draw a selection right on the photo, choose an aspect ratio (Free, 1:1, 4:3, 3:2,
16:9, 2:3, 3:4, 9:16), fine-tune its position and size, then crop, copy, or save the result.
- **Resize image**: resize by pixels or percentage, keep the proportions locked, pick a resampling
method (Nearest, Triangle, CatmullRom, Lanczos3, Gaussian), and see the new size before you apply it.
- **Frame navigation**: previous frame, pause/play, next frame (also `,` / `.` and `P`).
- **Color picker**: zoom in and sample any pixel to read its value in RGBA, HEX, HSL, HSV, CMYK,
or CIELAB, right on top of the image.
- **HDR tone mapper**: tone-map HDR images for a standard display, with live control over exposure,
reference white, highlight compression, and saturation.
- **Print, save as PDF**: print to any printer or save as PDF, with layouts from a full page to
contact sheets (2×2, 3×3, 4×4) and a live preview of every page.
- **Projector**: present on another display while you browse from the main window.

Editing opens the image at full resolution in an edit view (Esc or Cancel discards changes;
Save writes a PNG). The crop, resize, color picker, and tone mapper tools work only on images
that are **not** inside an archive; **to edit an image from an archive**, save it first
(Save page as image...), then open the saved file. Print / save as PDF and the projector also work on
archive file.

## Animation limitations

All animation frames are stored in memory. (max. approx. 128 MB per animation, 1500 frames).
Larger animations are truncated, and the status bar displays "truncated". Multi-page TIFF files
are treated as animations (one frame per page).

## Development

```bash
cargo test            # unit tests: natural order, ZIP/RAR, animated GIF, multi-page TIFF, HEIC/AVIF, memory budget, slider, zoom,
# wallpaper layout, desktop detection, gsettings/KDE/pcmanfm/sway commands, dual-page view, last-read positions,
# color conversions, crop/resize math, HDR tone mapping, minimal PDF generation
cargo run -- /path/to/image
```
