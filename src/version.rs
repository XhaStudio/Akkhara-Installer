//! Version strings: reading `akk --version` output and comparing release tags.

use std::cmp::Ordering;

/// First version number in `text` ("akk 1.3.0", "akk v2.2.0-alpha.2 (build)"),
/// including a pre-release suffix such as `-alpha.2`.
pub fn parse_version(text: &str) -> Option<String> {
    let c: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < c.len() {
        let starts = c[i].is_ascii_digit()
            && (i == 0
                || !c[i - 1].is_alphanumeric()
                || (matches!(c[i - 1], 'v' | 'V') && (i == 1 || !c[i - 2].is_alphanumeric())));
        if !starts {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < c.len() && (c[j].is_ascii_digit() || c[j] == '.') {
            j += 1;
        }
        let num: String = c[i..j].iter().collect::<String>().trim_end_matches('.').to_string();
        let valid = num.contains('.') && num.split('.').all(|p| !p.is_empty());
        if valid {
            let k = i + num.chars().count();
            if k < c.len() && (c[k] == '-' || c[k] == '+') && c.get(k + 1).map_or(false, |x| x.is_ascii_alphanumeric()) {
                let mut e = k + 1;
                while e < c.len() && (c[e].is_ascii_alphanumeric() || c[e] == '.' || c[e] == '-') {
                    e += 1;
                }
                let suffix: String = c[k..e].iter().collect();
                return Some(format!("{num}{}", suffix.trim_end_matches(['.', '-'])));
            }
            return Some(num);
        }
        i = j.max(i + 1);
    }
    None
}

/// ("1.3.0", "alpha.2") from "v1.3.0-alpha.2".
fn split_pre(v: &str) -> (String, String) {
    let v = v.trim().trim_start_matches(['v', 'V']).to_lowercase();
    match v.find(['-', '+']) {
        Some(i) => (v[..i].to_string(), v[i + 1..].to_string()),
        None => (v, String::new()),
    }
}

fn cmp_nums(a: &str, b: &str) -> Ordering {
    let n = |s: &str| -> Vec<u64> { s.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    let (x, y) = (n(a), n(b));
    for i in 0..x.len().max(y.len()) {
        let (p, q) = (*x.get(i).unwrap_or(&0), *y.get(i).unwrap_or(&0));
        if p != q {
            return p.cmp(&q);
        }
    }
    Ordering::Equal
}

/// Is `latest` newer than `installed`? (a final release beats its own pre-release)
pub fn is_newer(latest: &str, installed: &str) -> bool {
    let ((a, ap), (b, bp)) = (split_pre(latest), split_pre(installed));
    match cmp_nums(&a, &b) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => ap.is_empty() && !bp.is_empty(),
    }
}

/// Same release? ("v1.3.0" == "1.3.0" == "1.3", but "2.2.0-alpha.2" != "2.2.0")
pub fn same_version(a: &str, b: &str) -> bool {
    let ((an, ap), (bn, bp)) = (split_pre(a), split_pre(b));
    cmp_nums(&an, &bn) == Ordering::Equal && ap == bp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_versions_from_output() {
        assert_eq!(parse_version("akk 1.3.0").as_deref(), Some("1.3.0"));
        assert_eq!(parse_version("akk v1.3.0\n").as_deref(), Some("1.3.0"));
        assert_eq!(parse_version("Akkhara 1.10.4.").as_deref(), Some("1.10.4"));
        assert_eq!(parse_version("akk v2.2.0-alpha.2 (build 7)").as_deref(), Some("2.2.0-alpha.2"));
        assert_eq!(parse_version("akk 2.0.0-rc1").as_deref(), Some("2.0.0-rc1"));
        assert_eq!(parse_version("no version here"), None);
        assert_eq!(parse_version("version 3"), None);
    }

    #[test]
    fn newer_and_prerelease() {
        assert!(is_newer("v1.10.0", "1.9.0"));
        assert!(!is_newer("1.3.0", "v1.3.0"));
        assert!(!is_newer("1.2.9", "1.3.0"));
        assert!(is_newer("v2.0.0", "v2.0.0-alpha.2"));
        assert!(!is_newer("v2.0.0-alpha.2", "2.0.0"));
        assert!(!is_newer("v2.0.0-alpha.2", "2.0.0-alpha.2"));
    }

    #[test]
    fn same_release_detection() {
        assert!(same_version("v1.3.0", "1.3.0"));
        assert!(same_version("v1.3", "1.3.0"));
        assert!(same_version("V2.2.0-Alpha.2", "2.2.0-alpha.2"));
        assert!(!same_version("v2.2.0-alpha.2", "2.2.0"));
        assert!(!same_version("v1.3.1", "1.3.0"));
    }
}
