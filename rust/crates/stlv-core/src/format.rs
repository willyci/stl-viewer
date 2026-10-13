//! Text formatting shared by the metadata panel and status bar.

/// "512 Bytes", "1.5 KB", "2.25 MB" (binary units, like the Python app).
pub fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} Bytes")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Integer with comma thousands separators: 1234567 -> "1,234,567".
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Float with comma thousands separators and fixed decimals (Python's `{:,.2f}`).
pub fn thousands_f(x: f64, decimals: usize) -> String {
    let s = format!("{:.*}", decimals, x.abs());
    let (int_part, frac_part) = s.split_once('.').unwrap_or((&s, ""));
    let mut out = String::new();
    if x.is_sign_negative() && s.chars().any(|c| c.is_ascii_digit() && c != '0') {
        out.push('-');
    }
    out.push_str(&thousands(int_part.parse().unwrap_or(0)));
    if !frac_part.is_empty() {
        out.push('.');
        out.push_str(frac_part);
    }
    out
}

/// Milliseconds as "MM:SS" (minutes are not wrapped at 60, like the Python app).
pub fn format_mmss(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(format_file_size(0), "0 Bytes");
        assert_eq!(format_file_size(1023), "1023 Bytes");
        assert_eq!(format_file_size(1536), "1.5 KB");
        assert_eq!(format_file_size(3 * 1024 * 1024 / 2), "1.50 MB");
    }

    #[test]
    fn separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands_f(15550.5, 1), "15,550.5");
        assert_eq!(thousands_f(15.5505, 2), "15.55");
        assert_eq!(thousands_f(1234567.891, 2), "1,234,567.89");
    }

    #[test]
    fn mmss() {
        assert_eq!(format_mmss(0), "00:00");
        assert_eq!(format_mmss(61_999), "01:01");
        assert_eq!(format_mmss(3_600_000), "60:00");
    }
}
