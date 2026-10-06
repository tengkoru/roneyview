//! Preferensi ringan (mode zoom, bar, mode hemat memori, dua halaman, wallpaper) di
//! ~/.config/roneyview/state.json. Roneyview TIDAK menyimpan gambar/posisi dari sesi
//! sebelumnya. Semua galat I/O diabaikan dengan aman.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::wallpaper::WpPrefs;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum FitMode {
    /// Muat ke jendela, hanya memperkecil (tidak memperbesar gambar kecil).
    #[default]
    Fit,
    /// Muat ke jendela, gambar kecil ikut diperbesar.
    FitUpscale,
    Width,
    Height,
    Original,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub fit: FitMode,
    /// Bar navigasi bawah selalu tampil (centang "Kunci").
    pub bar_locked: bool,
    /// Mode hemat memori: hanya gambar yang sedang dilihat disimpan di RAM.
    pub low_memory: bool,
    /// Tampilkan dua halaman berdampingan.
    pub two_page: bool,
    /// Arah baca kanan-ke-kiri (manga).
    pub rtl: bool,
    /// Pada mode dua halaman, halaman pertama (sampul) tampil sendiri.
    pub cover_alone: bool,
    /// Pilihan terakhir di jendela "Set as wallpaper".
    pub wallpaper: WpPrefs,
}

impl Default for State {
    fn default() -> Self {
        State {
            fit: FitMode::default(),
            bar_locked: false,
            low_memory: false,
            two_page: false,
            rtl: false,
            cover_alone: true,
            wallpaper: WpPrefs::default(),
        }
    }
}

pub struct Store {
    path: Option<PathBuf>,
    pub state: State,
    /// Berkas lama (versi <= 0.4) masih memuat riwayat sesi; tulis ulang untuk menghapusnya.
    pub had_legacy_history: bool,
}

fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("roneyview"))
}

pub fn config_path(name: &str) -> Option<PathBuf> {
    config_dir().map(|d| d.join(name))
}

fn has_legacy_history(raw: &str) -> bool {
    raw.contains("\"positions\"") || raw.contains("\"last_origin\"")
}

impl Store {
    pub fn load() -> Self {
        let path = config_path("state.json");
        let raw = path.as_ref().and_then(|p| fs::read_to_string(p).ok());
        let had_legacy_history = raw.as_deref().is_some_and(has_legacy_history);
        let state = raw
            .and_then(|s| serde_json::from_str::<State>(&s).ok())
            .unwrap_or_default();
        Store {
            path,
            state,
            had_legacy_history,
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(&self.state).map_err(io::Error::other)?;
        // Tulis atomik: berkas sementara lalu rename.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Store {
            path: None,
            state: State::default(),
            had_legacy_history: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn berkas_lama_dengan_riwayat_dimuat_tanpa_gagal_dan_riwayat_hilang_saat_disimpan() {
        let old = r#"{"last_origin":"/foto","fit":"Width","bar_locked":true,
            "positions":[{"key":"/foto","index":4,"name":"a.jpg"}]}"#;
        assert!(has_legacy_history(old));
        let st: State = serde_json::from_str(old).unwrap();
        assert_eq!(st.fit, FitMode::Width);
        assert!(st.bar_locked);
        assert!(st.cover_alone, "bawaan baru harus berlaku untuk berkas lama");
        let again = serde_json::to_string(&st).unwrap();
        assert!(!has_legacy_history(&again), "{again}");
        assert!(!again.contains("/foto"));
    }

    #[test]
    fn json_rusak_atau_field_asing_tidak_membuat_gagal() {
        let st: State = serde_json::from_str(r#"{"fit":"Width","bidang_baru":1}"#).unwrap();
        assert_eq!(st.fit, FitMode::Width);
        assert!(serde_json::from_str::<State>("{rusak").is_err());
        let _ = Store::in_memory();
    }

    #[test]
    fn pilihan_dua_halaman_dan_wallpaper_bertahan_bolak_balik() {
        let st = State {
            two_page: true,
            rtl: true,
            cover_alone: false,
            wallpaper: WpPrefs {
                color: [1, 2, 3],
                ..WpPrefs::default()
            },
            ..State::default()
        };
        let back: State = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        assert!(back.two_page && back.rtl && !back.cover_alone);
        assert_eq!(back.wallpaper.color, [1, 2, 3]);
    }
}
