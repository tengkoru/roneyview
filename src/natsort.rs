//! Pengurutan "alami": img2 < img10, tidak peka huruf besar/kecil.

use std::cmp::Ordering;
use std::iter::Peekable;
use std::str::Chars;

pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ia = a.chars().peekable();
    let mut ib = b.chars().peekable();
    loop {
        match (ia.peek().copied(), ib.peek().copied()) {
            (None, None) => return a.cmp(b), // tie-break deterministik
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let da = take_digits(&mut ia);
                    let db = take_digits(&mut ib);
                    let o = cmp_digits(&da, &db);
                    if o != Ordering::Equal {
                        return o;
                    }
                } else {
                    let o = x.to_lowercase().cmp(y.to_lowercase());
                    if o != Ordering::Equal {
                        return o;
                    }
                    ia.next();
                    ib.next();
                }
            }
        }
    }
}

fn take_digits(it: &mut Peekable<Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(&c) = it.peek() {
        if c.is_ascii_digit() {
            s.push(c);
            it.next();
        } else {
            break;
        }
    }
    s
}

fn cmp_digits(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut v: Vec<&str>) -> Vec<&str> {
        v.sort_by(|a, b| natural_cmp(a, b));
        v
    }

    #[test]
    fn angka_dibandingkan_sebagai_nilai() {
        assert_eq!(
            sorted(vec!["img10.jpg", "img2.jpg", "Img1.jpg", "img100.jpg"]),
            vec!["Img1.jpg", "img2.jpg", "img10.jpg", "img100.jpg"]
        );
    }

    #[test]
    fn nol_di_depan_dan_folder() {
        assert_eq!(
            sorted(vec!["ch2/p10.png", "ch2/p9.png", "ch10/p1.png", "ch1/p001.png"]),
            vec!["ch1/p001.png", "ch2/p9.png", "ch2/p10.png", "ch10/p1.png"]
        );
    }

    #[test]
    fn angka_sangat_panjang_tidak_overflow() {
        let a = "x99999999999999999999999999999999";
        let b = "x100000000000000000000000000000000";
        assert_eq!(natural_cmp(a, b), Ordering::Less);
    }

    #[test]
    fn string_kosong_dan_sama() {
        assert_eq!(natural_cmp("", ""), Ordering::Equal);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
        assert_eq!(natural_cmp("a", "a"), Ordering::Equal);
    }
}
