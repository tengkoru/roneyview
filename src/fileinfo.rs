//! Informasi berkas untuk jendela Properties: ukuran, format, tanggal ubah,
//! serta jalur arsip + jalur di dalam arsip.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::i18n::{Lang, tr, tr_fmt};
use crate::source::Listing;

/// Konversi detik-sejak-epoch (UTC) ke (tahun, bulan, hari, jam, menit, detik).
/// Algoritma "civil from days" (Howard Hinnant), berlaku untuk tanggal Gregorian.
pub fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn format_stamp(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> String {
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// Selisih zona waktu lokal terhadap UTC (detik) pada saat `unix`.
fn local_offset_secs(unix: i64) -> i64 {
    // SAFETY: `localtime_r` menulis ke `tm` milik kita sendiri dan thread-safe.
    unsafe {
        let t: libc::time_t = unix as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        #[allow(clippy::unnecessary_cast)] // tipe tm_gmtoff berbeda antar platform
        {
            tm.tm_gmtoff as i64
        }
    }
}

/// "2026-10-05 14:03:09" dalam zona waktu lokal.
pub fn format_system_time(t: SystemTime) -> String {
    let unix = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let local = unix + local_offset_secs(unix);
    let (y, mo, d, h, mi, s) = civil_from_unix(local);
    format_stamp(y, mo, d, h, mi, s)
}

pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{:.2} GB", b / (KB * KB * KB))
    } else if b >= KB * KB {
        format!("{:.2} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Data halaman yang dibutuhkan Properties (disalin dari halaman yang sudah dimuat).
pub struct PageFacts {
    pub width: u32,
    pub height: u32,
    pub file_size: u64,
    pub format: String,
}

/// Lokasi gambar: berkas biasa, atau entri di dalam arsip.
pub enum Where {
    File(PathBuf),
    Archive { archive: PathBuf, inner: String },
}

pub fn locate(listing: &Listing, index: usize) -> Option<Where> {
    let name = listing.names.get(index)?;
    Some(if listing.is_archive() {
        Where::Archive {
            archive: listing.origin.clone(),
            inner: name.clone(),
        }
    } else {
        Where::File(listing.origin.join(name))
    })
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Baris (label, nilai) untuk jendela Properties.
pub fn properties_rows(listing: &Listing, index: usize, facts: &PageFacts, lang: Lang) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    let Some(loc) = locate(listing, index) else {
        return rows;
    };
    match &loc {
        Where::File(p) => {
            rows.push((tr(lang, "prop_filename").to_string(), file_name(p)));
            rows.push((
                tr(lang, "prop_location").to_string(),
                p.parent().map(|d| d.display().to_string()).unwrap_or_default(),
            ));
        }
        Where::Archive { archive, inner } => {
            rows.push((tr(lang, "prop_filename").to_string(), inner.rsplit('/').next().unwrap_or(inner).to_string()));
            rows.push((tr(lang, "prop_archive").to_string(), archive.display().to_string()));
            rows.push((tr(lang, "prop_inner_path").to_string(), inner.clone()));
        }
    }
    rows.push((tr(lang, "prop_imgsize").to_string(), tr_fmt(lang, "prop_dimensions", &[&facts.width, &facts.height])));
    rows.push((
        tr(lang, "prop_filesize").to_string(),
        format!("{} ({} byte)", human_size(facts.file_size), facts.file_size),
    ));
    rows.push((tr(lang, "prop_format").to_string(), facts.format.clone()));
    let modified = match &loc {
        Where::File(p) => std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .map(format_system_time),
        Where::Archive { .. } => listing.modified.get(index).cloned().flatten(),
    };
    rows.push((
        tr(lang, "prop_modified").to_string(),
        modified.unwrap_or_else(|| tr(lang, "prop_unknown").to_string()),
    ));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tanggal_dihitung_benar_untuk_titik_acuan() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil_from_unix(951_782_400), (2000, 2, 29, 0, 0, 0)); // tahun kabisat
        assert_eq!(civil_from_unix(1_000_000_000), (2001, 9, 9, 1, 46, 40));
        assert_eq!(civil_from_unix(1_780_000_000), (2026, 5, 28, 20, 26, 40));
        assert_eq!(civil_from_unix(-1), (1969, 12, 31, 23, 59, 59));
        assert_eq!(format_stamp(2026, 1, 2, 3, 4, 5), "2026-01-02 03:04:05");
    }

    #[test]
    fn ukuran_berkas_terbaca_manusiawi() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.00 MB");
        assert_eq!(human_size(3 << 30), "3.00 GB");
    }

    #[test]
    fn waktu_lokal_diformat_tanpa_panik() {
        let s = format_system_time(SystemTime::now());
        assert_eq!(s.len(), 19, "{s}");
        assert_eq!(&s[4..5], "-");
        let _ = format_system_time(UNIX_EPOCH);
    }
}
