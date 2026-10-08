//! Sumber gambar: folder biasa atau arsip ZIP/CBZ.
//! `Listing` hanya berisi daftar nama (cepat dibuat); byte gambar dibaca
//! belakangan oleh thread pemuat lewat `Reader`.

use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use zip::ZipArchive;

use crate::natsort::natural_cmp;

#[cfg(feature = "heif")]
pub const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "png", "gif", "webp", "bmp", "tif", "tiff", "qoi", "tga", "ico", "avif",
    "heic", "heif", "hif",
];
#[cfg(not(feature = "heif"))]
pub const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "png", "gif", "webp", "bmp", "tif", "tiff", "qoi", "tga", "ico",
];
#[cfg(feature = "rar")]
pub const ARCHIVE_EXTS: &[&str] = &["zip", "cbz", "rar", "cbr"];
#[cfg(not(feature = "rar"))]
pub const ARCHIVE_EXTS: &[&str] = &["zip", "cbz"];

/// Batas ukuran satu berkas gambar yang mau dibaca ke memori.
pub const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

fn ext_lower(name: &str) -> Option<String> {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

pub fn is_image_name(name: &str) -> bool {
    ext_lower(name).is_some_and(|e| IMAGE_EXTS.contains(&e.as_str()))
}

pub fn is_archive_path(path: &Path) -> bool {
    path.to_str()
        .and_then(ext_lower)
        .is_some_and(|e| ARCHIVE_EXTS.contains(&e.as_str()))
}

/// Waktu DOS (dipakai RAR): tanggal di 16 bit atas, jam di 16 bit bawah.
/// Sudah berupa waktu lokal si pembuat arsip, jadi tidak dikonversi zona waktu.
#[cfg(feature = "rar")]
fn dos_stamp(packed: u32) -> Option<String> {
    let (date, time) = ((packed >> 16) & 0xFFFF, packed & 0xFFFF);
    let (day, month, year) = (date & 31, (date >> 5) & 15, 1980 + (date >> 9));
    if !(1..=31).contains(&day) || !(1..=12).contains(&month) {
        return None;
    }
    let (sec, min, hour) = ((time & 31) * 2, (time >> 5) & 63, (time >> 11) & 31);
    Some(crate::fileinfo::format_stamp(i64::from(year), month, day, hour, min, sec))
}

fn file_name_string(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

enum Kind {
    Folder { files: Vec<PathBuf> },
    Archive { zip_indices: Vec<usize> },
    /// `ordinals`: urutan entri di dalam arsip RAR (RAR hanya bisa dibaca berurutan).
    #[cfg(feature = "rar")]
    Rar { ordinals: Vec<usize> },
}

/// Kursor pemrosesan unrar: berada tepat sebelum header entri berikutnya.
#[cfg(feature = "rar")]
type RarCursor = unrar::OpenArchive<unrar::Process, unrar::CursorBeforeHeader>;

pub struct Listing {
    /// Folder atau berkas arsip yang dibuka.
    pub origin: PathBuf,
    /// Nama tampilan tiap gambar, sudah terurut alami.
    pub names: Vec<String>,
    /// Tanggal ubah tiap entri ARSIP ("2026-10-05 14:03:09"); kosong untuk folder
    /// (tanggal berkas folder dibaca langsung dari disk saat dibutuhkan).
    pub modified: Vec<Option<String>>,
    kind: Kind,
}

pub enum Reader {
    Folder(Vec<PathBuf>),
    Zip {
        archive: Box<ZipArchive<BufReader<File>>>,
        indices: Vec<usize>,
    },
    #[cfg(feature = "rar")]
    Rar {
        path: PathBuf,
        ordinals: Vec<usize>,
        /// (ordinal entri berikutnya, kursor). Membaca halaman berurutan jadi O(1),
        /// termasuk pada arsip "solid"; mundur membuka ulang arsip dari awal.
        cursor: Option<(usize, RarCursor)>,
    },
}

impl Listing {
    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_archive(&self) -> bool {
        !matches!(self.kind, Kind::Folder { .. })
    }

    /// Buka path apa pun: folder, arsip, atau satu berkas gambar.
    /// Untuk berkas gambar, mengembalikan juga indeks berkas itu di dalam foldernya.
    pub fn open(path: &Path) -> Result<(Listing, Option<usize>), String> {
        let meta = fs::metadata(path)
            .map_err(|e| format!("Tidak dapat mengakses {}: {e}", path.display()))?;
        if meta.is_dir() {
            return Ok((list_folder(path, None)?, None));
        }
        if is_archive_path(path) {
            return Ok((list_archive(path)?, None));
        }
        let name = file_name_string(path);
        if !is_image_name(&name) {
            return Err(format!("Format berkas tidak didukung: {name}"));
        }
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        // Jalur relatif terhadap folder (mis. "sub/foto.png") supaya cocok dengan
        // `names` hasil pemindaian rekursif.
        let rel = path
            .strip_prefix(dir)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| name.clone());
        let listing = list_folder(dir, Some(rel.as_str()))?;
        let idx = listing.names.iter().position(|n| *n == rel).unwrap_or(0);
        Ok((listing, Some(idx)))
    }

    pub fn open_reader(&self) -> Result<Reader, String> {
        match &self.kind {
            Kind::Folder { files } => Ok(Reader::Folder(files.clone())),
            Kind::Archive { zip_indices } => {
                let file = File::open(&self.origin)
                    .map_err(|e| format!("Tidak dapat membuka arsip: {e}"))?;
                let archive = ZipArchive::new(BufReader::new(file))
                    .map_err(|e| format!("Arsip ZIP tidak valid: {e}"))?;
                Ok(Reader::Zip {
                    archive: Box::new(archive),
                    indices: zip_indices.clone(),
                })
            }
            #[cfg(feature = "rar")]
            Kind::Rar { ordinals } => Ok(Reader::Rar {
                path: self.origin.clone(),
                ordinals: ordinals.clone(),
                cursor: None,
            }),
        }
    }
}

impl Reader {
    pub fn read(&mut self, i: usize) -> Result<Vec<u8>, String> {
        match self {
            Reader::Folder(paths) => {
                let p = paths
                    .get(i)
                    .ok_or_else(|| "Indeks gambar di luar jangkauan".to_string())?;
                let len = fs::metadata(p)
                    .map_err(|e| format!("Gagal membaca {}: {e}", p.display()))?
                    .len();
                if len > MAX_ENTRY_BYTES {
                    return Err(format!("Berkas terlalu besar ({} MB)", len >> 20));
                }
                fs::read(p).map_err(|e| format!("Gagal membaca {}: {e}", p.display()))
            }
            Reader::Zip { archive, indices } => {
                let zi = *indices
                    .get(i)
                    .ok_or_else(|| "Indeks gambar di luar jangkauan".to_string())?;
                let file = archive
                    .by_index(zi)
                    .map_err(|e| format!("Gagal membuka entri arsip: {e}"))?;
                if file.size() > MAX_ENTRY_BYTES {
                    return Err(format!("Entri terlalu besar ({} MB)", file.size() >> 20));
                }
                let mut buf = Vec::with_capacity(file.size().min(MAX_ENTRY_BYTES) as usize);
                // `take` menjaga dari ZIP bom yang berbohong soal ukuran.
                file.take(MAX_ENTRY_BYTES + 1)
                    .read_to_end(&mut buf)
                    .map_err(|e| format!("Gagal mengekstrak entri: {e}"))?;
                if buf.len() as u64 > MAX_ENTRY_BYTES {
                    return Err("Entri terlalu besar".to_string());
                }
                Ok(buf)
            }
            #[cfg(feature = "rar")]
            Reader::Rar {
                path,
                ordinals,
                cursor,
            } => {
                let target = *ordinals
                    .get(i)
                    .ok_or_else(|| "Indeks gambar di luar jangkauan".to_string())?;
                // Lanjutkan dari kursor lama bila target masih di depannya.
                let (mut ordinal, mut cur) = match cursor.take() {
                    Some((next, c)) if next <= target => (next, c),
                    _ => {
                        let c = unrar::Archive::new(path)
                            .open_for_processing()
                            .map_err(|e| rar_error("Tidak dapat membuka arsip RAR", &e))?;
                        (0, c)
                    }
                };
                loop {
                    let header = cur
                        .read_header()
                        .map_err(|e| rar_error("Gagal membaca header RAR", &e))?
                        .ok_or_else(|| "Entri tidak ditemukan di arsip RAR".to_string())?;
                    if ordinal == target {
                        let entry = header.entry();
                        if entry.unpacked_size > MAX_ENTRY_BYTES {
                            return Err(format!(
                                "Entri terlalu besar ({} MB)",
                                entry.unpacked_size >> 20
                            ));
                        }
                        let (data, rest) = header
                            .read()
                            .map_err(|e| rar_error("Gagal mengekstrak entri RAR", &e))?;
                        *cursor = Some((ordinal + 1, rest));
                        return Ok(data);
                    }
                    cur = header
                        .skip()
                        .map_err(|e| rar_error("Gagal melompati entri RAR", &e))?;
                    ordinal += 1;
                }
            }
        }
    }
}

#[cfg(feature = "rar")]
fn rar_error(context: &str, e: &unrar::error::UnrarError) -> String {
    let text = e.to_string();
    if text.to_ascii_lowercase().contains("password") {
        "Arsip RAR berpassword/terenkripsi tidak didukung".to_string()
    } else {
        format!("{context}: {text}")
    }
}

/// Pindai folder SECARA REKURSIF: gambar di sub-folder ikut dimuat.
/// `names` berisi jalur relatif terhadap `dir` ("sub/foto.png") supaya berkas
/// bernama sama di folder berbeda tetap bisa dibedakan.
fn list_folder(dir: &Path, keep_hidden: Option<&str>) -> Result<Listing, String> {
    let mut items: Vec<(String, PathBuf)> = Vec::new();
    collect_images(dir, dir, keep_hidden, &mut items)?;
    if items.is_empty() {
        return Err(format!(
            "Tidak ada gambar yang didukung di {}",
            dir.display()
        ));
    }
    items.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    let (names, files): (Vec<_>, Vec<_>) = items.into_iter().unzip();
    Ok(Listing {
        origin: dir.to_path_buf(),
        names,
        modified: Vec::new(),
        kind: Kind::Folder { files },
    })
}

fn collect_images(
    root: &Path,
    dir: &Path,
    keep_hidden: Option<&str>,
    items: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    let rd = fs::read_dir(dir)
        .map_err(|e| format!("Tidak dapat membaca folder {}: {e}", dir.display()))?;
    for entry in rd.flatten() {
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().into_owned());
        let Ok(rel) = rel else { continue };
        // `file_type` tidak mengikuti symlink: symlink ke folder tidak diikuti
        // (mencegah loop), symlink ke berkas tetap dibuka lewat `is_file`.
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            let base = rel.rsplit('/').next().unwrap_or(rel.as_str());
            // Lewati folder tersembunyi (.git, .thumbnails, ...) supaya tidak
            // memindai ribuan berkas yang tidak diinginkan.
            if base.starts_with('.') {
                continue;
            }
            collect_images(root, &path, keep_hidden, items)?;
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let base = rel.rsplit('/').next().unwrap_or(rel.as_str());
        if base.starts_with('.') && keep_hidden != Some(rel.as_str()) {
            continue;
        }
        if !is_image_name(base) {
            continue;
        }
        items.push((rel, path));
    }
    Ok(())
}

#[derive(PartialEq)]
enum ArchiveType {
    Zip,
    Rar,
}

/// Kenali jenis arsip dari isi berkas (bukan ekstensi): banyak ".cbz" ternyata RAR
/// dan sebaliknya.
fn sniff_archive(path: &Path) -> Option<ArchiveType> {
    let mut head = [0u8; 8];
    let mut f = File::open(path).ok()?;
    let n = f.read(&mut head).ok()?;
    let head = &head[..n];
    if head.starts_with(b"Rar!\x1a\x07") {
        Some(ArchiveType::Rar)
    } else if head.starts_with(b"PK") {
        Some(ArchiveType::Zip)
    } else {
        None
    }
}

fn list_archive(path: &Path) -> Result<Listing, String> {
    match sniff_archive(path) {
        Some(ArchiveType::Rar) => list_rar(path),
        _ => list_zip(path),
    }
}

#[cfg(not(feature = "rar"))]
fn list_rar(_path: &Path) -> Result<Listing, String> {
    Err("Dukungan RAR tidak ikut dikompilasi (bangun dengan fitur \"rar\")".to_string())
}

#[cfg(feature = "rar")]
fn list_rar(path: &Path) -> Result<Listing, String> {
    let archive = unrar::Archive::new(path)
        .open_for_listing()
        .map_err(|e| rar_error("Arsip RAR tidak dapat dibuka", &e))?;
    let mut items: Vec<(String, usize, Option<String>)> = Vec::new();
    for (ordinal, entry) in archive.enumerate() {
        // Header rusak di tengah arsip: pakai entri yang sudah terbaca.
        let Ok(entry) = entry else { break };
        if entry.is_directory() {
            continue;
        }
        let name = entry.filename.to_string_lossy().replace('\\', "/");
        let base = name.rsplit('/').next().unwrap_or(&name);
        if name.starts_with("__MACOSX/") || base.starts_with('.') {
            continue;
        }
        if is_image_name(&name) {
            items.push((name, ordinal, dos_stamp(entry.file_time)));
        }
    }
    if items.is_empty() {
        return Err(format!(
            "Tidak ada gambar yang didukung di dalam {}",
            file_name_string(path)
        ));
    }
    items.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    let mut names = Vec::with_capacity(items.len());
    let mut ordinals = Vec::with_capacity(items.len());
    let mut modified = Vec::with_capacity(items.len());
    for (n, o, m) in items {
        names.push(n);
        ordinals.push(o);
        modified.push(m);
    }
    Ok(Listing {
        origin: path.to_path_buf(),
        names,
        modified,
        kind: Kind::Rar { ordinals },
    })
}

fn list_zip(path: &Path) -> Result<Listing, String> {
    let file = File::open(path).map_err(|e| format!("Tidak dapat membuka arsip: {e}"))?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|e| format!("Arsip ZIP tidak valid: {e}"))?;
    let mut items: Vec<(String, usize, Option<String>)> = Vec::new();
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index_raw(i) else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let base = name.rsplit('/').next().unwrap_or(&name);
        if name.starts_with("__MACOSX/") || base.starts_with('.') {
            continue;
        }
        if is_image_name(&name) {
            let stamp = entry.last_modified().map(|d| {
                crate::fileinfo::format_stamp(
                    i64::from(d.year()),
                    u32::from(d.month()),
                    u32::from(d.day()),
                    u32::from(d.hour()),
                    u32::from(d.minute()),
                    u32::from(d.second()),
                )
            });
            items.push((name, i, stamp));
        }
    }
    if items.is_empty() {
        return Err(format!(
            "Tidak ada gambar yang didukung di dalam {}",
            file_name_string(path)
        ));
    }
    items.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    let mut names = Vec::with_capacity(items.len());
    let mut zip_indices = Vec::with_capacity(items.len());
    let mut modified = Vec::with_capacity(items.len());
    for (n, i, m) in items {
        names.push(n);
        zip_indices.push(i);
        modified.push(m);
    }
    Ok(Listing {
        origin: path.to_path_buf(),
        names,
        modified,
        kind: Kind::Archive { zip_indices },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("roneyview-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn ekstensi_dikenali_tanpa_peduli_huruf() {
        assert!(is_image_name("A.JPG"));
        assert!(is_image_name("x.WebP"));
        assert!(!is_image_name("catatan.txt"));
        assert!(!is_image_name("tanpa_ekstensi"));
        assert!(is_archive_path(Path::new("/a/b/komik.CBZ")));
        assert!(!is_archive_path(Path::new("/a/b/komik.7z")));
        assert_eq!(is_archive_path(Path::new("/a/b/komik.CBR")), cfg!(feature = "rar"));
        assert_eq!(is_image_name("foto.HEIC"), cfg!(feature = "heif"));
        assert_eq!(is_image_name("foto.avif"), cfg!(feature = "heif"));
    }

    #[test]
    fn folder_diurutkan_alami_dan_berkas_dibuka_pada_indeks_benar() {
        let d = tmpdir("folder");
        for n in ["p10.png", "p2.png", "p1.png", ".tersembunyi.png", "baca.txt"] {
            fs::write(d.join(n), b"x").unwrap();
        }
        let (l, start) = Listing::open(&d.join("p2.png")).unwrap();
        assert_eq!(l.names, vec!["p1.png", "p2.png", "p10.png"]);
        assert_eq!(start, Some(1));
        assert!(!l.is_archive());
        let (l2, s2) = Listing::open(&d).unwrap();
        assert_eq!(l2.len(), 3);
        assert_eq!(s2, None);
        let mut r = l.open_reader().unwrap();
        assert_eq!(r.read(0).unwrap(), b"x");
        assert!(r.read(99).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn folder_dipindai_rekursif_dengan_nama_relatif() {
        let d = tmpdir("rekursif");
        fs::create_dir_all(d.join("sub/deep")).unwrap();
        fs::create_dir_all(d.join(".hidden_dir")).unwrap();
        for n in [
            "a.png",
            "sub/b2.png",
            "sub/b10.png",
            "sub/deep/c.png",
            ".hidden_dir/d.png",
            ".x.png",
            "catatan.txt",
        ] {
            fs::write(d.join(n), b"x").unwrap();
        }
        let (l, _) = Listing::open(&d).unwrap();
        assert_eq!(
            l.names,
            vec!["a.png", "sub/b2.png", "sub/b10.png", "sub/deep/c.png"]
        );
        // Membuka satu berkas: listing berakar di folder induk berkas itu
        // (rekursif ke bawah), dan indeksnya menunjuk berkas yang benar.
        let (l2, start) = Listing::open(&d.join("sub/b10.png")).unwrap();
        assert_eq!(l2.names, vec!["b2.png", "b10.png", "deep/c.png"]);
        assert_eq!(start, Some(1));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn nama_sama_di_subfolder_berbeda_tetap_dibedakan() {
        let d = tmpdir("duplikat");
        fs::create_dir_all(d.join("s1")).unwrap();
        fs::create_dir_all(d.join("s2")).unwrap();
        fs::write(d.join("s1/foto.png"), b"satu").unwrap();
        fs::write(d.join("s2/foto.png"), b"dua").unwrap();
        let (l, _) = Listing::open(&d).unwrap();
        assert_eq!(l.names, vec!["s1/foto.png", "s2/foto.png"]);
        let mut r = l.open_reader().unwrap();
        assert_eq!(r.read(0).unwrap(), b"satu");
        assert_eq!(r.read(1).unwrap(), b"dua");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn folder_kosong_dan_format_salah_memberi_galat_bukan_panik() {
        let d = tmpdir("kosong");
        assert!(Listing::open(&d).is_err());
        fs::write(d.join("a.txt"), b"x").unwrap();
        assert!(Listing::open(&d.join("a.txt")).is_err());
        assert!(Listing::open(&d.join("tidak-ada.png")).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn arsip_zip_dibaca_terurut_dan_isi_benar() {
        let d = tmpdir("zip");
        let zpath = d.join("komik.cbz");
        {
            let f = File::create(&zpath).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            for (n, body) in [
                ("bab1/10.png", "sepuluh"),
                ("bab1/2.png", "dua"),
                ("__MACOSX/bab1/2.png", "sampah"),
                ("bab1/.hidden.png", "sampah"),
                ("bab1/catatan.txt", "sampah"),
            ] {
                w.start_file(n, opts).unwrap();
                w.write_all(body.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        let (l, _) = Listing::open(&zpath).unwrap();
        assert!(l.is_archive());
        assert_eq!(l.names, vec!["bab1/2.png", "bab1/10.png"]);
        let mut r = l.open_reader().unwrap();
        assert_eq!(r.read(0).unwrap(), b"dua");
        assert_eq!(r.read(1).unwrap(), b"sepuluh");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn zip_rusak_memberi_galat() {
        let d = tmpdir("ziprusak");
        let p = d.join("rusak.zip");
        fs::write(&p, b"bukan zip sungguhan").unwrap();
        assert!(Listing::open(&p).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(feature = "rar")]
    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/")).join(name)
    }

    #[cfg(feature = "rar")]
    #[test]
    fn rar_terurut_alami_dan_bisa_dibaca_acak() {
        let (l, _) = Listing::open(&fixture("mini.rar")).unwrap();
        assert!(l.is_archive());
        assert_eq!(l.names, vec!["a1.png", "a2.png", "a10.png"]);
        let mut r = l.open_reader().unwrap();
        for i in [2usize, 0, 1, 1, 0, 2] {
            let b = r.read(i).unwrap();
            assert_eq!(&b[..4], b"\x89PNG", "entri {i}");
        }
        assert!(r.read(99).is_err());
    }

    #[cfg(feature = "rar")]
    #[test]
    fn rar_solid_berurutan_maju_mundur_dan_isi_sama_dengan_non_solid() {
        let (a, _) = Listing::open(&fixture("mini.rar")).unwrap();
        let (b, _) = Listing::open(&fixture("solid.rar")).unwrap();
        assert_eq!(a.names, b.names);
        let mut ra = a.open_reader().unwrap();
        let mut rb = b.open_reader().unwrap();
        // maju berurutan (jalur kursor cepat), lalu mundur (buka ulang), lalu lompat
        for i in [0usize, 1, 2, 0, 2, 1] {
            assert_eq!(ra.read(i).unwrap(), rb.read(i).unwrap(), "entri {i}");
        }
    }

    #[cfg(feature = "rar")]
    #[test]
    fn arsip_dikenali_dari_isi_bukan_ekstensi() {
        let d = tmpdir("sniff");
        // RAR bernama .cbz
        let salah = d.join("sebenarnya-rar.cbz");
        fs::copy(fixture("mini.rar"), &salah).unwrap();
        let (l, _) = Listing::open(&salah).unwrap();
        assert_eq!(l.len(), 3);
        // ZIP bernama .cbr
        let zip_path = d.join("sebenarnya-zip.cbr");
        {
            let f = File::create(&zip_path).unwrap();
            let mut w = zip::ZipWriter::new(f);
            w.start_file("x.png", zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(b"data").unwrap();
            w.finish().unwrap();
        }
        let (l2, _) = Listing::open(&zip_path).unwrap();
        assert_eq!(l2.names, vec!["x.png"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(feature = "rar")]
    #[test]
    fn rar_rusak_memberi_galat_bukan_panik() {
        let d = tmpdir("rarrusak");
        let bytes = fs::read(fixture("mini.rar")).unwrap();
        let p1 = d.join("terpotong.rar");
        fs::write(&p1, &bytes[..bytes.len() / 3]).unwrap();
        // boleh gagal membuka atau membuka sebagian, tetapi tidak boleh panik
        if let Ok((l, _)) = Listing::open(&p1) {
            if let Ok(mut r) = l.open_reader() {
                for i in 0..l.len() {
                    let _ = r.read(i);
                }
            }
        }
        let p2 = d.join("sampah.rar");
        fs::write(&p2, b"Rar!\x1a\x07\x01\x00 ini bukan rar sungguhan").unwrap();
        assert!(Listing::open(&p2).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(feature = "rar")]
    #[test]
    fn waktu_dos_didekode_dan_nilai_ngawur_ditolak() {
        // 2026-10-05 14:03:08 -> tanggal ((2026-1980)<<9)|(10<<5)|5, jam (14<<11)|(3<<5)|(8/2)
        let date: u32 = ((2026 - 1980) << 9) | (10 << 5) | 5;
        let time: u32 = (14 << 11) | (3 << 5) | 4;
        assert_eq!(dos_stamp((date << 16) | time).as_deref(), Some("2026-10-05 14:03:08"));
        assert_eq!(dos_stamp(0), None);
        assert_eq!(dos_stamp(0xFFFF_FFFF), None); // bulan 15 tidak ada
    }

    #[test]
    fn tanggal_entri_zip_terbaca() {
        let d = tmpdir("zipdate");
        let zpath = d.join("a.zip");
        {
            let f = File::create(&zpath).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let t = zip::DateTime::from_date_and_time(2024, 3, 7, 9, 5, 30).unwrap();
            let opts = zip::write::SimpleFileOptions::default().last_modified_time(t);
            w.start_file("x.png", opts).unwrap();
            w.write_all(b"d").unwrap();
            w.finish().unwrap();
        }
        let (l, _) = Listing::open(&zpath).unwrap();
        assert_eq!(l.modified, vec![Some("2024-03-07 09:05:30".to_string())]);
        let _ = fs::remove_dir_all(&d);
    }
}
