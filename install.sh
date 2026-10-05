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
  rm -f "$BIN" "$DESKTOP" "$ICON"
  refresh_caches
  echo "Roneyview dihapus. (Pengaturan di ~/.config/roneyview dibiarkan.)"
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
echo "Fitur yang dipakai : ${FEATS:-(tidak ada, hanya inti)}"
if ((${#missing[@]})); then
  echo "Dilewati karena paket belum terpasang:"
  printf '  - %s\n' "${missing[@]}"
fi
[[ "${1:-}" == "--dry-run" ]] && exit 0

cargo build --release --locked --no-default-features --features "$FEATS"
install -Dm755 target/release/roneyview "$BIN"
install -Dm644 packaging/roneyview.desktop "$DESKTOP"
install -Dm644 packaging/roneyview.svg "$ICON"
refresh_caches

echo "Selesai. Jalankan: roneyview   (pastikan ~/.local/bin ada di PATH)"
