//! Lokalisasi Roneyview: Indonesia (bawaan) dan Inggris.
//!
//! Cara kerja:
//! - Di kode, teks UI ditulis sebagai KUNCI, misal `tr(lang, "menu_file")`.
//! - `i18n/language_id.txt` memetakan kunci -> teks Indonesia.
//! - `i18n/language_en.txt` memetakan kunci -> teks Inggris.
//! - Ganti bahasa = lookup kunci yang sama di file bahasa yang aktif.
//!
//! Kedua file disematkan ke biner via `include_str!`, jadi tidak menambah
//! dependensi dan biner tetap bisa disalin ke komputer lain apa adanya.
//! Kalau suatu kunci belum ada terjemahannya, dipakai teks Indonesianya.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Lang {
    #[default]
    Id,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Id => "id",
            Lang::En => "en",
        }
    }

    pub fn from_code(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "en" | "english" | "inggris" => Lang::En,
            _ => Lang::Id,
        }
    }

    /// Nama bahasa untuk ditampilkan di menu.
    pub fn display_name(self) -> &'static str {
        match self {
            Lang::Id => "Indonesia",
            Lang::En => "English",
        }
    }
}

fn parse_table(text: &'static str) -> HashMap<&'static str, &'static str> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once(" = ") {
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() && !v.is_empty() {
                out.insert(k, v);
            }
        }
    }
    out
}

/// Ganti `\n` literal di nilai tabel menjadi baris baru.
/// Dipakai untuk teks multi-baris seperti `empty_hint`.
pub fn unescape(s: &str) -> String {
    s.replace("\\n", "\n")
}

static ID_TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
static EN_TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

fn id_table() -> &'static HashMap<&'static str, &'static str> {
    ID_TABLE.get_or_init(|| parse_table(include_str!("i18n/language_id.txt")))
}

fn en_table() -> &'static HashMap<&'static str, &'static str> {
    EN_TABLE.get_or_init(|| parse_table(include_str!("i18n/language_en.txt")))
}

/// Terjemahkan KUNCI ke bahasa aktif.
/// Urutan fallback: bahasa aktif -> Indonesia -> kunci itu sendiri.
pub fn tr(lang: Lang, key: &str) -> &str {
    let table = match lang {
        Lang::Id => id_table(),
        Lang::En => en_table(),
    };
    if let Some(v) = table.get(key) {
        return v;
    }
    // Fallback ke Indonesia bila terjemahan Inggris belum ada.
    if lang == Lang::En {
        if let Some(v) = id_table().get(key) {
            return v;
        }
    }
    key
}

/// Seperti `tr`, tapi untuk template ber-argumen: `{}` diganti berurutan.
/// Contoh: `tr_fmt(lang, "prop_dimensions", &[&w, &h])`.
pub fn tr_fmt(lang: Lang, key: &str, args: &[&dyn Display]) -> String {
    let t = tr(lang, key);
    let mut out = String::with_capacity(t.len() + 32);
    let mut ai = args.iter();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' && chars.peek() == Some(&'}') {
            chars.next();
            match ai.next() {
                Some(a) => out.push_str(&a.to_string()),
                None => out.push_str("{}"),
            }
        } else if c == '{' {
            // Placeholder bernama seperti {name}: biarkan, diganti manual via .replace()
            out.push(c);
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kedua_tabel_terparse() {
        assert!(!id_table().is_empty());
        assert!(!en_table().is_empty());
    }

    #[test]
    fn indonesia_memakai_file_id() {
        assert_eq!(tr(Lang::Id, "menu_file"), "Berkas");
    }

    #[test]
    fn inggris_memakai_file_en() {
        assert_eq!(tr(Lang::En, "menu_file"), "File");
    }

    #[test]
    fn fallback_ke_indonesia() {
        // Kunci yang tidak ada di en -> tampil Indonesia
        assert_eq!(tr(Lang::En, "key_tidak_ada"), "key_tidak_ada");
    }

    #[test]
    fn tr_fmt_mengganti_urutan() {
        let s = tr_fmt(Lang::Id, "prop_dimensions", &[&2, &10]);
        assert_eq!(s, "2 x 10 piksel");
        let s = tr_fmt(Lang::En, "prop_dimensions", &[&2, &10]);
        assert_eq!(s, "2 x 10 pixels");
    }
}
