//! Secret-material scanning for runner artifacts.
//!
//! The runner collects its own credential values at startup, then sweeps the
//! files it writes — risk snapshots, the execution journal — for any of them
//! before order flow starts and again at shutdown. A hit aborts the session
//! naming the artifact and the credential's label, never the value itself.
//!
//! Credentials come from an operator-managed secret manager injected as
//! environment variables; nothing here reads or writes a secret to disk.

/// The set of credential substrings to keep out of artifacts.
///
/// Values shorter than 8 characters are ignored: they cannot be told apart
/// from incidental text and would only produce false positives.
#[derive(Debug, Default)]
pub struct SecretScan {
    needles: Vec<(String, String)>,
}

impl SecretScan {
    /// Build a scanner from `(label, value)` pairs. Blank and short values
    /// are dropped, so a partially configured environment scans what exists
    /// instead of failing to scan.
    pub fn from_values<'a, I>(values: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let mut needles = Vec::new();
        for (label, value) in values {
            let value = value.trim();
            if value.len() >= 8 && !needles.iter().any(|(_, n)| n == value) {
                needles.push((label.to_string(), value.to_string()));
            }
        }
        Self { needles }
    }

    /// Add per-line needles from a PEM private key body. Header, footer, and
    /// short lines are skipped; base64 payloads are matched line by line
    /// because artifact writers escape newlines inside JSON strings.
    pub fn add_pem(&mut self, label: &str, pem: &str) {
        for line in pem.lines() {
            let line = line.trim();
            if line.len() >= 16
                && !line.starts_with("-----")
                && !self.needles.iter().any(|(_, n)| n == line)
            {
                self.needles
                    .push((format!("{label} pem body"), line.to_string()));
            }
        }
    }

    /// True when nothing is registered (e.g. a dry-run without credentials).
    pub fn is_empty(&self) -> bool {
        self.needles.is_empty()
    }

    /// Labels whose secret appears in `bytes`. Empty means clean.
    pub fn scan_bytes(&self, bytes: &[u8]) -> Vec<&str> {
        self.needles
            .iter()
            .filter(|(_, needle)| bytes.windows(needle.len()).any(|w| w == needle.as_bytes()))
            .map(|(label, _)| label.as_str())
            .collect()
    }

    /// Scan one file; unreadable files are reported as errors, not skipped.
    pub fn scan_file(&self, path: &std::path::Path) -> std::io::Result<Vec<String>> {
        let bytes = std::fs::read(path)?;
        Ok(self
            .scan_bytes(&bytes)
            .into_iter()
            .map(str::to_string)
            .collect())
    }

    /// Scan every path; the first leak aborts with a message that names the
    /// artifact and the credential label only.
    pub fn assert_files_clean(&self, paths: &[&std::path::Path]) -> Result<(), String> {
        for path in paths {
            if !path.exists() {
                continue;
            }
            if let Some(label) = self
                .scan_file(path)
                .map_err(|e| format!("scan {}: {e}", path.display()))?
                .first()
            {
                return Err(format!(
                    "{} contains material from credential `{label}`",
                    path.display()
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_ID: &str = "kalshi-live-key-id-0042";
    const POLY_KEY: &str = "poly-api-key-deadbeef123456";

    #[test]
    fn short_and_blank_values_never_become_needles() {
        let scan = SecretScan::from_values([
            ("a", ""),
            ("b", "short"),
            ("c", "seventeen-characters"),
            ("d", KEY_ID),
            ("e", KEY_ID), // duplicate value collapses
        ]);
        assert_eq!(scan.needles.len(), 2);
    }

    #[test]
    fn scanner_finds_planted_material_and_reports_only_the_label() {
        let scan = SecretScan::from_values([("POLY_API_KEY", POLY_KEY)]);
        assert_eq!(scan.scan_bytes(b"clean frame data"), Vec::<&str>::new());
        let leaked = format!(r#"{{"order":"x","note":"{POLY_KEY}"}}"#);
        assert_eq!(scan.scan_bytes(leaked.as_bytes()), vec!["POLY_API_KEY"]);
    }

    #[test]
    fn pem_body_lines_are_needles_headers_are_not() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIabc123def456ghi789==\nshort\n-----END RSA PRIVATE KEY-----\n";
        let mut scan = SecretScan::default();
        scan.add_pem("KALSHI_PRIVATE_KEY", pem);
        assert_eq!(scan.needles.len(), 1);
        assert_eq!(scan.needles[0].0, "KALSHI_PRIVATE_KEY pem body");

        // A journal-style escaped string still trips the line needle.
        let artifact = format!(r#"{{"pem":"{}"}}"#, "MIIabc123def456ghi789==");
        assert_eq!(
            scan.scan_bytes(artifact.as_bytes()),
            vec!["KALSHI_PRIVATE_KEY pem body"]
        );
    }

    #[test]
    fn file_scan_names_path_and_label_but_not_the_value() {
        let dir = std::env::temp_dir().join(format!("arbkit-secret-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clean = dir.join("clean.ndjson");
        let dirty = dir.join("dirty.ndjson");
        std::fs::write(&clean, "session frames only").unwrap();
        std::fs::write(&dirty, format!("journal line with {KEY_ID} inside")).unwrap();

        let scan = SecretScan::from_values([("KALSHI_ACCESS_KEY_ID", KEY_ID)]);
        assert!(scan.assert_files_clean(&[&clean]).is_ok());

        let error = scan.assert_files_clean(&[&dirty]).unwrap_err();
        assert!(error.contains("dirty.ndjson"));
        assert!(error.contains("KALSHI_ACCESS_KEY_ID"));
        assert!(!error.contains(KEY_ID));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_files_are_not_errors() {
        let scan = SecretScan::from_values([("KALSHI_ACCESS_KEY_ID", KEY_ID)]);
        assert!(scan
            .assert_files_clean(&[std::path::Path::new("/nonexistent/artifact")])
            .is_ok());
    }
}
