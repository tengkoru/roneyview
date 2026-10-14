//! Posisi baca terakhir, satu per folder/arsip (bukan satu posisi global).
//!
//! Disimpan di `~/.config/roneyview/positions.json`, terpisah dari
//! `state.json` supaya berkas pengaturan tetap kecil. Entri paling baru ada
//! di depan; jumlah entri dibatasi agar berkas tidak membengkak.
//!
//! Kunci tiap entri adalah path kanonis folder/arsip yang dibuka, jadi
//! mengganti nama atau memindah folder dianggap lokasi yang berbeda
//! (posisi lama tidak dipakai).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::config_path;

/// Batas jumlah folder/arsip yang diingat.
const MAX_ENTRIES: usize = 200;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    key: String,
    index: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct FileData {
    #[serde(default)]
    entries: Vec<Entry>,
}

#[derive(Debug, Default)]
pub struct PositionStore {
    path: Option<PathBuf>,
    /// Entri paling baru di depan (urutan LRU).
    entries: Vec<Entry>,
}

/// Kunci penyimpanan untuk sebuah daftar: path kanonis dari folder/arsip
/// asal. Gagal kanoniskan (mis. path sudah dihapus) -> pakai path apa adanya.
pub fn key_for(origin: &Path) -> String {
    fs::canonicalize(origin)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| origin.to_string_lossy().into_owned())
}

impl PositionStore {
    pub fn load() -> Self {
        let path = config_path("positions.json");
        let mut entries = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<FileData>(&s).ok())
            .map(|d| d.entries)
            .unwrap_or_default();
        entries.truncate(MAX_ENTRIES);
        PositionStore { path, entries }
    }

    pub fn get(&self, key: &str) -> Option<usize> {
        self.entries.iter().find(|e| e.key == key).map(|e| e.index)
    }

    /// Catat posisi; entri yang sudah ada dipindah ke depan.
    pub fn set(&mut self, key: String, index: usize) {
        self.entries.retain(|e| e.key != key);
        self.entries.insert(0, Entry { key, index });
        self.entries.truncate(MAX_ENTRIES);
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let data = FileData { entries: self.entries.clone() };
        let json = serde_json::to_string_pretty(&data).map_err(io::Error::other)?;
        // Tulis atomik: berkas sementara lalu rename.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        PositionStore { path: None, entries: Vec::new() }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simpan_dan_ambil_kembali() {
        let mut s = PositionStore::in_memory();
        assert_eq!(s.get("/komik/a.cbz"), None);
        s.set("/komik/a.cbz".into(), 7);
        assert_eq!(s.get("/komik/a.cbz"), Some(7));
    }

    #[test]
    fn per_folder_bukan_global() {
        let mut s = PositionStore::in_memory();
        s.set("/komik/a.cbz".into(), 7);
        s.set("/foto".into(), 2);
        assert_eq!(s.get("/komik/a.cbz"), Some(7));
        assert_eq!(s.get("/foto"), Some(2));
    }

    #[test]
    fn update_memindah_ke_depan() {
        let mut s = PositionStore::in_memory();
        s.set("a".into(), 1);
        s.set("b".into(), 2);
        s.set("a".into(), 9);
        assert_eq!(s.get("a"), Some(9));
        assert_eq!(s.entries[0].key, "a");
        assert_eq!(s.entries.len(), 2);
    }

    #[test]
    fn dibatasi_200_entri() {
        let mut s = PositionStore::in_memory();
        for i in 0..250 {
            s.set(format!("/f{i}"), i);
        }
        assert_eq!(s.len(), MAX_ENTRIES);
        assert_eq!(s.get("/f249"), Some(249));
        assert_eq!(s.get("/f0"), None, "entri paling lama terbuang");
    }

    #[test]
    fn roundtrip_json() {
        let mut s = PositionStore::in_memory();
        s.set("/komik/a.cbz".into(), 7);
        let json = serde_json::to_string(&FileData { entries: s.entries.clone() }).unwrap();
        let back: FileData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].index, 7);
    }

    #[test]
    fn key_for_tidak_panik_untuk_path_hilang() {
        let k = key_for(Path::new("/path/yang/tidak/ada/xyz"));
        assert!(!k.is_empty());
    }
}
