//! Penyimpanan state ringan (posisi terakhir per sumber, mode zoom) di
//! ~/.config/roneyview/state.json. Semua galat I/O diabaikan dengan aman.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const MAX_POSITIONS: usize = 200;

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

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Position {
    pub key: String,
    pub index: usize,
    pub name: String,
}

#[derive(Default, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub last_origin: Option<String>,
    pub fit: FitMode,
    /// Bar navigasi bawah selalu tampil (centang "Kunci").
    pub bar_locked: bool,
    /// Terbaru di depan.
    pub positions: Vec<Position>,
}

pub struct Store {
    path: Option<PathBuf>,
    pub state: State,
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("roneyview").join("state.json"))
}

impl Store {
    pub fn load() -> Self {
        let path = config_path();
        let state = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<State>(&s).ok())
            .unwrap_or_default();
        Store { path, state }
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

    pub fn position_for(&self, key: &str) -> Option<&Position> {
        self.state.positions.iter().find(|p| p.key == key)
    }

    pub fn remember(&mut self, key: &str, index: usize, name: &str) {
        self.state.positions.retain(|p| p.key != key);
        self.state.positions.insert(
            0,
            Position {
                key: key.to_string(),
                index,
                name: name.to_string(),
            },
        );
        self.state.positions.truncate(MAX_POSITIONS);
        self.state.last_origin = Some(key.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_menaruh_terbaru_di_depan_dan_membatasi_jumlah() {
        let mut s = Store { path: None, state: State::default() };
        s.remember("a", 1, "x");
        s.remember("b", 2, "y");
        s.remember("a", 5, "z");
        assert_eq!(s.state.positions.len(), 2);
        assert_eq!(s.state.positions[0].key, "a");
        assert_eq!(s.position_for("a").unwrap().index, 5);
        for i in 0..500 {
            s.remember(&format!("k{i}"), i, "n");
        }
        assert_eq!(s.state.positions.len(), MAX_POSITIONS);
        assert_eq!(s.state.last_origin.as_deref(), Some("k499"));
    }

    #[test]
    fn json_rusak_atau_field_asing_tidak_membuat_gagal() {
        let st: State = serde_json::from_str(r#"{"fit":"Width","bidang_baru":1}"#).unwrap();
        assert_eq!(st.fit, FitMode::Width);
        assert!(serde_json::from_str::<State>("{rusak").is_err());
    }
}
