# Roneyview (version 0.5.0)

Penampil gambar alternatif Honeyview untuk Linux, ditulis dengan bahasa pemrograman Rust (egui/eframe).

## Apa yang Roneyview bisa lakukan?
- **Membuka gambar**, **folder**, dan **archive**
- **Mendukung format gambar**: JPEG/JPG, PNG, GIF, WebP, BMP, TIFF, QOI, TGA, ICO, AVIF, HEIC/HEIF
- **Mendukung format gambar beranimasi**: GIF, WebP, dan APNG
- **Tekan tombol 'P' untuk jeda/putar file format gambar beranimasi**
- **Mendukung format archive**: ZIP/CBZ/RAR/CBR; hanya menampilkan file gambar yang terdapat pada file archive
- **Tidak menyimpan riwayat gambar terakhir yang dilihat**: tidak ada daftar gambar/posisi dari sesi sebelumnya; jika kamu menjalakan roneyview pada command-line tanpa mencantumkan lokasi file/folder yang ingin dibuka maka Roneyview hanya akan membuka jendela kosong
- **Klik kanan pada gambar akan menampilkan menu berikut**:
    - **Set as wallpaper...**
    - **Properties**
    - **Tindakan**: 
        - Buka didalam folder
        - pindahkan ke sampah
- **Mode dua halaman** dan **arah baca kanan-ke-kiri** untuk manga/komik
- Mode muat jendela / lebar / tinggi / 100%, zoom ke arah kursor, geser, rotasi, layar penuh 
- **Tombol prev/next pada tepi kiri/kanan**; hanya akan muncul pada saat pointer mendekati tepi
- **Bar bawah**:
    - **tombol prev/next** 
    - **slider lompat**  
    - **checklist 'kunci' untuk terus menampilkan 'bar bawah'**

## Debian/Ubuntu/MX Linux/turunan debian lainnya

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
Semua alat bantu untuk kelancaran aplikasi roneyview akan diunduh otomatis saat Anda menjalankan ./install.sh. 
Namun apabila ingin melihat alat bantu apa saja yang diperlukan oleh Roneyview, Anda dapat melihatnya [disini](supporting-roneyview.md)


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

RAR hanya bisa dibaca berurutan (bukan akses acak). Roneyview mengingat posisi baca di dalam arsip selama dibuka, jadi membuka
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
| `M` | Mode hemat memori |
| Enter, F11, klik ganda | Layar penuh (Esc keluar) |
| Ctrl+O / Ctrl+Shift+O | Buka berkas atau arsip / buka folder |
| F1 | Daftar pintasan |
| Ctrl+Q | Keluar |

## Pemakaian baris perintah

```bash
roneyview photo.jpg        # buka foto, seluruh folder ikut dimuat
roneyview ~/Comic_title/Chapter1    # buka folder
roneyview title_comic.cbz       # buka arsip (juga .zip, .rar, .cbr)
roneyview                 # jendela kosong; Ctrl+O untuk membuka gambar
```

Hanya **pilihan tampilan** yang disimpan di `~/.config/roneyview/state.json` (mode zoom, Kunci bar, mode hemat
memori, mode dua halaman, arah baca, dan pilihan terakhir di jendela wallpaper). File lama (versi <= 0.4) yang
masih berisi riwayat posisi akan ditulis ulang tanpa riwayat itu saat Roneyview dijalankan.

```bash
roneyview --set-wallpaper photo.jpg --style zoomed --color '#202020'   # tanpa membuka jendela
roneyview --restore-wallpaper                                          # pulihkan wallpaper root window
```

## Batas memori

Roneyview membatasi memori sendiri supaya tidak menyeret sistem ke kehabisan RAM:

- Ukuran tekstur per halaman dibatasi **sisi terpanjang** (batas GPU) dan **jumlah piksel** (maks. 4 sampai 16 MP,
  disesuaikan dengan RAM tersedia saat start). Foto 50 MP ditampilkan dari versi yang diperkecil.
- Cache halaman punya anggaran total 96 sampai 256 MB (termasuk mipmap); halaman terjauh dibuang lebih dulu.
- Berkas kecil yang **mengaku** berukuran raksasa (bom dekompresi) ditolak sebelum dekode dengan pesan jelas.
- Bila memori tersedia sistem turun di bawah 256 MB, prefetch dimatikan dan cache dikosongkan.

### Mode hemat memori (tombol `M` atau menu Tampilan)

Hanya gambar yang **sedang dilihat** yang disimpan di RAM; gambar sebelumnya dibuang begitu Anda pindah,
dan tidak ada prefetch. Pilihan ini diingat antar sesi. Harganya: pindah halaman terasa lebih lambat pada foto
besar karena setiap gambar harus didekode saat dibuka (termasuk saat kembali ke gambar sebelumnya).
Pada uji 24 foto 24 MP, pemakaian RAM stabil turun dari sekitar 395 MB ke 232 MB.

### Yang tidak dilakukan Roneyview

- Roneyview tidak memakai bantuan tumblerd atau thumbnailer sistem apa pun, dan tidak membuat thumbnail; gambar didekode sendiri
- Tidak ada cache gambar di disk dan tidak ada riwayat gambar yang dibuka. Berkas yang ditulis hanya
  `~/.config/roneyview/state.json` (pilihan tampilan) dan, bila Anda memakai *Set as wallpaper*, salinan gambar
  wallpaper di `~/.local/share/roneyview/`. Saat Roneyview ditutup, seluruh memorinya dilepas oleh sistem.

## Menu klik kanan

Klik kanan pada gambar (pada mode dua halaman: pada halaman yang diklik):

- **Set as wallpaper...** membuka jendela baru dengan pratinjau langsung,
  **Color** (**Solid color** dengan pemilih warna + kolom hex di sebelahnya, atau **Transparent**) dan **Style**
  (*None, Centered, Tiled, Stretched, Scaled, Zoomed*), lalu tombol **Apply**. Gambar dari ZIP/RAR atau format yang
  tidak dikenal desktop (HEIC, AVIF, WebP, ...) otomatis diekspor ke PNG sementara.
- **Properties** menampilkan nama berkas, lokasi, ukuran gambar, ukuran berkas, format, dan tanggal ubah. Untuk
  gambar di dalam arsip: jalur arsip dan jalur di dalam arsip.
- **Tindakan**: *Buka di dalam folder* (membuka file manager dan memilih berkasnya) serta *Pindahkan ke sampah*
  (dengan konfirmasi; mengikuti spesifikasi Trash freedesktop.org). Bila gambar berada di dalam arsip, keduanya
  hanya menampilkan peringatan dan Roneyview tidak melakukan tindakan lain.

### Set as wallpaper di berbagai desktop

Tidak ada satu cara yang berlaku di semua desktop, karena desktop environment mengelola latar belakangnya
sendiri. Roneyview mendeteksi desktop yang berjalan lalu memakai mekanisme resminya:

| Desktop | Cara | Status uji |
|---|---|---|
| Xfce | `xfconf-query` (properti xfce4-desktop) | diuji dengan xfdesktop 4.18 sungguhan |
| Window manager tanpa desktop manager (Fluxbox, Openbox, i3, ...) | melukis sendiri ke root window X11 (semua gaya, multi-monitor via RandR) + autostart `roneyview --restore-wallpaper` | diuji di Xvfb |
| GNOME, Budgie, Unity | `gsettings` (`org.gnome.desktop.background`) | kunci dan nilai divalidasi terhadap skema asli |
| Cinnamon | `gsettings` (`org.cinnamon.desktop.background`) | divalidasi terhadap skema asli |
| MATE | `gsettings` (`org.mate.background`) | divalidasi terhadap skema asli |
| KDE Plasma | skrip Plasma lewat D-Bus (`gdbus`) | **belum diuji di Plasma**; hanya baris perintahnya yang diperiksa |
| LXDE / LXQt | `pcmanfm --set-wallpaper` / `pcmanfm-qt` | **belum diuji**; hanya baris perintahnya yang diperiksa |
| Sway | `swaymsg output * bg` | **belum diuji**; hanya baris perintahnya yang diperiksa |
| Wayland lain, GNOME/KDE tanpa perintah di atas | tidak didukung | pesan jelas di jendela |

Catatan: *Transparent* hanya bermakna di Xfce; di desktop lain dipakai warna hitam.

## Mode dua halaman dan arah baca

- `D` menyalakan/mematikan **mode dua halaman**; `K` mengganti **arah baca** kiri-ke-kanan / kanan-ke-kiri (manga).
- Menu Tampilan juga punya **Halaman pertama tunggal (sampul)**: halaman pertama tampil sendiri, lalu berpasangan
  2-3, 4-5, ...
- Halaman **lebar** (lanskap, mis. halaman ganda hasil scan) selalu tampil sendiri; tinggi dua halaman disamakan.
- Pada arah kanan-ke-kiri halaman pertama berada di kanan, **panah kiri = berikutnya**, dan tombol sisi, bar,
  serta slider ikut dicerminkan. `Spasi`/`PgDn` tetap berarti berikutnya.
- Rotasi (`R`) tidak tersedia pada mode dua halaman.

## Batasan animasi

Semua frame animasi disimpan di memori (maks. sekitar 128 MB per animasi, 1500 frame).
Animasi yang lebih besar dipotong dan status bar menampilkan "dipotong".

## Pengembangan

```bash
cargo test            # unit test: urutan alami, ZIP/RAR, GIF animasi, HEIC/AVIF, anggaran memori, slider, zoom,
                      # tata letak wallpaper, deteksi desktop, perintah gsettings/KDE/pcmanfm/sway, dua halaman
cargo run -- /jalur/ke/gambar
```
