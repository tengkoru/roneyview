#!/usr/bin/env bash
# Menghapus Roneyview yang dipasang lewat install.sh (ke ~/.local).
#   ./uninstall.sh            -> tanya: hapus Roneyview saja, atau penuh
#   ./uninstall.sh --keep-deps -> hapus Roneyview saja (dependensi dibiarkan)
#   ./uninstall.sh --full     -> hapus Roneyview + dependensi
#   Tambahkan --yes untuk menjawab "ya" pada semua konfirmasi.
# Catatan: ./install.sh --uninstall masih tersedia untuk hapus aplikasi saja.
set -euo pipefail

BIN="$HOME/.local/bin/roneyview"
DESKTOP="$HOME/.local/share/applications/roneyview.desktop"
ICON="$HOME/.local/share/icons/hicolor/scalable/apps/roneyview.svg"
AUTOSTART="$HOME/.config/autostart/roneyview-wallpaper.desktop"

YES=0
MODE=""
for arg in "$@"; do
  case "$arg" in
    --keep-deps) MODE="keep" ;;
    --full) MODE="full" ;;
    --yes) YES=1 ;;
    -h|--help)
      echo "Pakai: $0 [--keep-deps | --full] [--yes]"
      echo "  --keep-deps  Hapus Roneyview saja; dependensi dibiarkan di perangkat."
      echo "  --full       Hapus Roneyview + dependensi (paket dev apt + cache crate)."
      echo "  --yes        Jawab 'ya' pada semua konfirmasi."
      exit 0
      ;;
    *)
      echo "Argumen tidak dikenal: $arg (lihat $0 --help)" >&2
      exit 1
      ;;
  esac
done

# confirm "pertanyaan" -> 0 bila ya (atau --yes), 1 bila tidak.
confirm() {
  local q="$1"
  if [ "$YES" -eq 1 ]; then return 0; fi
  local ans=""
  read -r -p "$q [y/N] " ans || ans=""
  case "$ans" in
    y|Y|ya|YA|Ya) return 0 ;;
    *) return 1 ;;
  esac
}

refresh_caches() {
  command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
}

remove_app() {
  rm -f "$BIN" "$DESKTOP" "$ICON" "$AUTOSTART"
  refresh_caches
  echo "Roneyview dihapus (biner, entri menu, ikon, autostart wallpaper)."
}

remove_deps() {
  # Perintah apt: langsung bila root, lewat sudo bila ada, atau kosong.
  local apt_cmd=()
  if [ "$(id -u)" -eq 0 ]; then
    apt_cmd=(apt)
  elif command -v sudo >/dev/null; then
    apt_cmd=(sudo apt)
  fi
  # Paket dev yang dipakai install.sh untuk kompilasi; hanya yang terpasang.
  local pkgs=(build-essential pkg-config libgtk-3-dev libheif-dev libheif-plugin-libde265 libheif-plugin-dav1d)
  local installed=()
  local p
  for p in "${pkgs[@]}"; do
    if dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q "install ok installed"; then
      installed+=("$p")
    fi
  done
  if ((${#installed[@]})); then
    echo "Paket yang akan dihapus: ${installed[*]}"
    echo "Perhatian: paket-paket ini mungkin dipakai program lain juga."
    if confirm "Lanjutkan hapus paket-paket di atas dengan apt?"; then
      if ((${#apt_cmd[@]})); then
        "${apt_cmd[@]}" remove --purge -y "${installed[@]}"
        "${apt_cmd[@]}" autoremove -y
      else
        echo "sudo tidak ditemukan; hapus manual: sudo apt remove --purge ${installed[*]}"
      fi
    else
      echo "Batal hapus paket apt; lanjut ke langkah berikutnya."
    fi
  else
    echo "Tidak ada paket dev Roneyview yang terpasang."
  fi
  # Cache crate yang diunduh saat kompilasi (cargo mengunduh ulang bila perlu).
  local cache_dirs=("$HOME/.cargo/registry" "$HOME/.cargo/git")
  local found=0
  local d
  for d in "${cache_dirs[@]}"; do
    if [ -d "$d" ]; then found=1; fi
  done
  if [ "$found" -eq 1 ]; then
    if confirm "Hapus cache crate yang diunduh (~/.cargo/registry dan ~/.cargo/git)?"; then
      rm -rf "${cache_dirs[@]}"
      echo "Cache crate dihapus."
    else
      echo "Cache crate dibiarkan."
    fi
  fi
  echo "Toolchain Rust (rustup) TIDAK dihapus otomatis, karena mungkin dipakai proyek lain."
  echo "Bila memang tidak dipakai lagi: rustup self uninstall"
}

if [ -z "$MODE" ]; then
  echo "Pilih mode uninstall:"
  echo "  1) Roneyview saja (dependensi dibiarkan di perangkat)"
  echo "  2) Penuh (Roneyview + dependensi: paket dev apt + cache crate)"
  echo "  0) Batal"
  choice=""
  read -r -p "Pilihan [0/1/2]: " choice || choice=""
  case "$choice" in
    1) MODE="keep" ;;
    2) MODE="full" ;;
    *) echo "Dibatalkan."; exit 0 ;;
  esac
fi

if [ "$MODE" = "keep" ]; then
  if confirm "Hapus Roneyview (biner, entri menu, ikon, autostart)?"; then
    remove_app
  else
    echo "Dibatalkan."
    exit 0
  fi
else
  echo "Mode penuh: Roneyview + dependensi akan dihapus."
  if confirm "Lanjutkan uninstall penuh?"; then
    remove_app
    remove_deps
  else
    echo "Dibatalkan."
    exit 0
  fi
fi

echo "Pengaturan di ~/.config/roneyview dan wallpaper di ~/.local/share/roneyview dibiarkan."
echo "Selesai."
