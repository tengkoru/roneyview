# Roneyview

Penampil gambar cepat alternatif Honeyview untuk Linux, ditulis dengan Rust (egui/eframe).

- Membuka satu gambar (folder induknya otomatis dimuat), folder, atau arsip **ZIP/CBZ/RAR/CBR**
- Format: JPEG, PNG, GIF, WebP, BMP, TIFF, QOI, TGA, ICO, AVIF, HEIC/HEIF
- Animasi: GIF, WebP, dan APNG (tombol `P` untuk jeda/putar)
- Jenis arsip dikenali dari isi berkas, bukan ekstensi (".cbz" yang ternyata RAR tetap terbuka)
- Urutan nama alami (`p2` sebelum `p10`), orientasi EXIF dihormati
- Prefetch halaman sekitar di thread terpisah; UI tidak pernah menunggu dekoder
- **Melanjutkan dari posisi terakhir** untuk setiap folder/arsip
- Mode muat jendela / lebar / tinggi / 100%, zoom ke arah kursor, geser, rotasi, layar penuh
- Idle hemat CPU (tidak menggambar ulang kalau tidak ada perubahan)
- **Tombol Sebelumnya/Berikutnya di tepi kiri/kanan** yang muncul perlahan hanya saat pointer mendekati tepi
- **Bar bawah**: tombol Sebelumnya/Berikutnya, **slider lompat** ke gambar mana pun (mis. 46/100) dengan
  petunjuk nama berkas saat digeser, dan centang **Kunci** agar bar tetap tampil
- Pemakaian memori dibatasi mengikuti RAM yang tersedia (lihat "Batas memori")

## Memasang di MX Linux atau turunan Debian lainnya

```bash
# 1. Alat build + pustaka untuk dialog berkas (GTK3) dan AVIF/HEIC (libheif)
sudo apt update
sudo apt install build-essential pkg-config libgtk-3-dev libheif-dev curl

# 2. Rust terbaru (paket rustc di Debian terlalu lama; butuh Rust >= 1.88)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 3. Kompilasi dan pasang ke ~/.local (tanpa sudo)
./install.sh
```

`install.sh` memeriksa paket yang tersedia dan hanya mengaktifkan fitur yang bisa dibangun,
lalu menampilkan fitur mana yang dilewati beserta perintah `apt` untuk memasangnya.
`./install.sh --dry-run` hanya menampilkan pilihan itu tanpa mengompilasi.

| Fitur | Butuh saat kompilasi | Catatan |
|---|---|---|
| `dialogs` (dialog Buka berkas) | `libgtk-3-dev` | tanpa ini tetap bisa seret-lepas dan argumen baris perintah |
| `heif` (AVIF, HEIC/HEIF) | `libheif-dev` **>= 1.17** | lihat catatan versi di bawah |
| `rar` (RAR/CBR) | `g++` (dari `build-essential`) | kode unrar dikompilasi otomatis oleh Cargo |

Setelah terpasang, `roneyview` tersedia di terminal (pastikan `~/.local/bin` ada di `PATH`)
dan muncul di menu aplikasi serta "Buka dengan" pada pengelola berkas.
Hapus dengan `./install.sh --uninstall`.

### Catatan versi libheif (AVIF/HEIC)

- **MX Linux 25 / Debian 13**: libheif 1.19, didukung. Bila HEIC atau AVIF gagal terbuka dengan
  pesan "dekoder", pasang plugin kodek: `sudo apt install libheif-plugin-libde265 libheif-plugin-dav1d`.
- **MX Linux 23 / Debian 12**: libheif hanya 1.15, **terlalu lama** untuk crate yang dipakai
  (butuh >= 1.17). `install.sh` otomatis melewati fitur ini; semua fitur lain tetap jalan.
- Dibangun manual tanpa fitur tertentu, contoh: `cargo build --release --locked --no-default-features --features dialogs,rar`

### Catatan RAR

RAR hanya bisa dibaca berurutan (bukan akses acak). Roneyview menyimpan posisi baca, jadi membuka
halaman berikutnya cepat, tetapi melompat mundur membuka ulang arsip dari awal. Pada arsip *solid*
yang besar, lompatan jauh bisa terasa lambat. Arsip berpassword tidak didukung.

## Pintasan

| Tombol | Fungsi |
|---|---|
| Kanan / PgDn / Spasi | Gambar berikutnya |
| Kiri / PgUp / Backspace | Gambar sebelumnya |
| Home / End | Pertama / terakhir |
| Roda mouse | Geser vertikal; pindah gambar saat sudah mentok |
| Atas / Bawah, seret kiri | Geser gambar |
| Ctrl + roda, `+`, `-` | Zoom |
| `F` / `Shift+F` | Muat ke jendela (kecilkan saja / perbesar juga) |
| `W` / `H` | Sesuaikan lebar / tinggi |
| `0` atau `1` | Ukuran asli 100% |
| `R` / `Shift+R` | Putar kanan / kiri |
| `P` | Jeda / putar animasi |
| `L` | Kunci / lepas bar bawah |
| Enter, F11, klik ganda | Layar penuh (Esc keluar) |
| Ctrl+O / Ctrl+Shift+O | Buka berkas atau arsip / buka folder |
| F1 | Daftar pintasan |
| Ctrl+Q | Keluar |

## Mengatur Roneview menjadi default pada beberapa format
``` bash
xdg-mime default roneyview.desktop image/jpg image/png image/jpeg image/heic image/heif image/gif image/webp image/avif application/zip application/vnd.comicbook+zip application/vnd.rar
```
## Pemakaian baris perintah

```bash
roneyview photo1.jpg        # buka foto, seluruh folder ikut dimuat
roneyview ~/Comic_Title/Chap1    # buka folder
roneyview Comic_Title.cbz       # buka arsip (juga .zip, .rar, .cbr)
roneyview                 # lanjut dari tempat terakhir
```

Pengaturan dan posisi terakhir disimpan di `~/.config/roneyview/state.json`.

## Tombol samping dan bar bawah

- Arahkan pointer ke **tepi kiri/kanan** jendela: muncul tombol panah. Tombol sebelumnya tidak muncul
  di gambar pertama, dan tombol berikutnya tidak muncul di gambar terakhir.
- Arahkan pointer ke **bagian bawah**: muncul bar berisi tombol, slider, penunjuk `46/100`, dan centang **Kunci**.
- **Slider**: tahan dan geser; halaman baru dibuka saat tombol mouse **dilepas** (bukan di setiap piksel
  geseran), jadi menyeret melewati ratusan foto besar tidak memuat semuanya. Klik pada slider melompat langsung.
  Selama ditahan, bar tidak menghilang walau pointer keluar dari area bar.
- **Kunci** (atau tombol `L`): bar tetap tampil walau pointer menjauh. Pilihan ini diingat antar sesi.
- Semua elemen ini ikut bekerja di mode layar penuh.

## Batas memori

Roneyview membatasi memori sendiri supaya tidak menyeret sistem ke kehabisan RAM:

- Ukuran tekstur per halaman dibatasi **sisi terpanjang** (batas GPU) dan **jumlah piksel** (maks. 4 sampai 16 MP,
  disesuaikan dengan RAM tersedia saat start). Foto 50 MP ditampilkan dari versi yang diperkecil.
- Cache halaman punya anggaran total 96 sampai 256 MB (termasuk mipmap); halaman terjauh dibuang lebih dulu.
- Berkas kecil yang **mengaku** berukuran raksasa (bom dekompresi) ditolak sebelum dekode dengan pesan jelas.
- Bila memori tersedia sistem turun di bawah 256 MB, prefetch dimatikan dan cache dikosongkan.

## Batasan animasi

Semua frame animasi disimpan di memori (maks. sekitar 128 MB per animasi, 1500 frame).
Animasi yang lebih besar dipotong dan status bar menampilkan "dipotong".

## Permasalah yang kami ketahui, solusi?
1. Layar menjadi hitam karena tumblerd menggunakan ram terus menerus setelah menjalankan Roneyview, terjadi pada file manager Thunar. 
Solusi: Buka aplikasi file manager thunar -> edit -> preferences ->  show thumbnail (Never). Cara ini akan mengurangi kemungkinan terjadinya layar hitam, bukan hilang sepenuhnya.


## Pengembangan

```bash
cargo test            # 37 unit test (urutan alami, ZIP/RAR, GIF animasi, HEIC/AVIF, anggaran memori, slider, zoom)
cargo run -- /jalur/ke/gambar
```
