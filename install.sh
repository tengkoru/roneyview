#!/usr/bin/env bash
# Memasang Roneyview untuk pengguna saat ini (tanpa sudo) ke ~/.local.
#   ./install.sh              -> kompilasi (fitur disesuaikan otomatis) lalu pasang
#   ./install.sh --dry-run    -> hanya tampilkan fitur yang akan dipakai
#   ./install.sh --uninstall  -> hapus
set -euo pipefail
cd "$(dirname "$0")"

BIN="$HOME/.local/bin/roneyview"
DESKTOP="$HOME/.local/share/applications/roneyview.desktop"
ICON="$HOME/.local/share/icons/hicolor/scalable/apps/roneyview.svg"

refresh_caches() {
  command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
}

if [[ "${1:-}" == "--uninstall" ]]; then
  rm -f "$BIN" "$DESKTOP" "$ICON" "$HOME/.config/autostart/roneyview-wallpaper.desktop"
  refresh_caches
  echo "Roneyview dihapus. (Pengaturan di ~/.config/roneyview dan wallpaper di ~/.local/share/roneyview dibiarkan.)"
  exit 0
fi

if ! command -v cargo >/dev/null; then
  echo "cargo tidak ditemukan. Pasang Rust terlebih dahulu: https://rustup.rs" >&2
  exit 1
fi

# Pilih fitur sesuai paket yang tersedia, supaya build tidak gagal di tengah jalan.
features=()
missing=()
have_pc() { command -v pkg-config >/dev/null; }

if have_pc && pkg-config --exists gtk+-3.0 2>/dev/null; then
  features+=(dialogs)
else
  missing+=("dialog berkas GTK  -> sudo apt install pkg-config libgtk-3-dev")
fi
if have_pc && pkg-config --atleast-version=1.17 libheif 2>/dev/null; then
  features+=(heif)
else
  missing+=("AVIF/HEIC (libheif >= 1.17) -> sudo apt install pkg-config libheif-dev")
fi
if command -v g++ >/dev/null; then
  features+=(rar)
else
  missing+=("RAR/CBR            -> sudo apt install build-essential")
fi

FEATS=$(IFS=,; echo "${features[*]:-}")
echo "Important dependency installed: ${FEATS:-(tidak ada, hanya inti)}"
if ((${#missing[@]})); then
  echo "Skipping, because dependency not installed:"
  printf '  - %s\n' "${missing[@]}"
fi
[[ "${1:-}" == "--dry-run" ]] && exit 0

# Progres selama kompilasi (cargo tidak menampilkannya bawaan):
# tiap baris "Membangun Compiling <crate> vX.Y.Z" punya bar-nya sendiri.
# Cargo tidak melaporkan progres per crate, jadi bar-nya indikator aktivitas
# yang beranimasi selama crate itu dikompilasi; tiap ganti crate, bar baru
# mulai lagi dari awal. Tidak ada persen (global maupun per crate).
LOG="$(mktemp /tmp/roneyview-build-XXXXXX.log)"
echo 'Prepare...'
if command -v stdbuf >/dev/null 2>&1; then
    stdbuf -oL -eL cargo build --release --locked --no-default-features --features "$FEATS" >"$LOG" 2>&1 &
else
    cargo build --release --offline --no-default-features --features "$FEATS" >"$LOG" 2>&1 &
fi
# NOTE: stdbuf -oL biar output cargo tidak ke-buffer saat di-redirect ke file.
# Kalau ke-buffer, log kelihatan kosong dan progres macet di "menyiapkan...".
CARGO_PID=$!
BAR_W=12
bar_penuh() { printf '%*s' "$BAR_W" '' | tr ' ' '#'; }
bar_frame() {
    # Blok 4 sel yang meluncur bolak-balik di dalam bar selebar BAR_W.
    local f=$1 span period p pos i bar=""
    span=$((BAR_W - 4))
    period=$((span * 2))
    p=$((f % period))
    pos=$p
    if [ "$p" -gt "$span" ]; then pos=$((period - p)); fi
    for ((i = 0; i < BAR_W; i++)); do
        if [ "$i" -ge "$pos" ] && [ "$i" -lt "$((pos + 4))" ]; then bar+='#'; else bar+='-'; fi
    done
    printf '[%s]' "$bar"
}
is_tty=0
if [ -t 1 ]; then is_tty=1; fi
last_crate=""
frame=0
while kill -0 "$CARGO_PID" 2>/dev/null; do
    # Tampilkan aktivitas terakhir cargo (Compiling, Checking, Fresh, dsb.)
    # kalau belum ada, tampilkan "menyiapkan..."
    crate=$(grep -E '^\s+(Compiling|Checking|Fresh|Downloading|Downloaded|Updating|Blocking)' "$LOG" 2>/dev/null | tail -1 | sed 's/^ *//;s/ *$//' | cut -c1-50 || true)
    if [ -z "$crate" ]; then crate=" start..."; fi
    if [ "$crate" != "$last_crate" ]; then
        # Crate sebelumnya selesai: tutup barisnya dengan bar penuh.
        if [ "$is_tty" -eq 1 ] && [ -n "$last_crate" ]; then
            printf '\rBuild %-50s [%s]\n' "$last_crate" "$(bar_penuh)"
        fi
        last_crate="$crate"
        frame=0
        # Di luar terminal (log/pipe), bar tidak bisa beranimasi:
        # cetak satu baris per crate, tanpa bar.
        if [ "$is_tty" -eq 0 ]; then
            printf 'Build %s\n' "$crate"
        fi
    fi
    if [ "$is_tty" -eq 1 ]; then
        printf '\rBuild %-50s %s' "$crate" "$(bar_frame "$frame")"
        frame=$((frame + 1))
    fi
    sleep 0.2
done
if wait "$CARGO_PID"; then
    if [ "$is_tty" -eq 1 ]; then
        printf '\rBuild %-50s [%s]\n' "roneyview (completed)" "$(bar_penuh)"
    else
        printf 'Build %s\n' "roneyview (completed)"
    fi
    rm -f "$LOG"
else
    if [ "$is_tty" -eq 1 ]; then printf '\n'; fi
    printf 'Compiled fail. 20 baris terakhir log:\n'
    tail -n 20 "$LOG"
    echo "full log: $LOG"
    exit 1
fi
install -Dm755 target/release/roneyview "$BIN"
install -Dm644 packaging/roneyview.desktop "$DESKTOP"
install -Dm644 packaging/roneyview.svg "$ICON"
refresh_caches

echo "Finish. Run roneyview with command: roneyview   (make sure ~/.local/bin in the PATH)"
