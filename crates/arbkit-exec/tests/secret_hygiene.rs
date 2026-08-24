//! Secret-hygiene gates: no credential material in repo, configs, or
//! runner artifacts (HJ-149).

use arbkit_exec::{KalshiConfig, PolymarketConfig, SecretScan};

const KALSHI_KEY_ID: &str = "kalshi-live-key-id-0042";
const POLY_L1: &str = "0xdeadbeef1234567890abcdefdeadbeef567890ab";
const POLY_KEY: &str = "poly-api-key-deadbeef123456";
const POLY_SECRET: &str = "poly-api-secret-base64ish-987654321";
const POLY_PASSPHRASE: &str = "poly-passphrase-hmac-input-24680";

#[test]
fn env_example_keeps_credential_values_blank() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env.example");
    let contents = std::fs::read_to_string(path).unwrap();

    for name in [
        "KALSHI_ACCESS_KEY_ID",
        "KALSHI_PRIVATE_KEY_PATH",
        "POLY_WALLET_ADDRESS",
        "POLY_PRIVATE_KEY",
        "POLY_API_KEY",
        "POLY_API_SECRET",
        "POLY_API_PASSPHRASE",
    ] {
        let line = contents
            .lines()
            .find(|l| l.starts_with(&format!("{name}=")))
            .unwrap_or_else(|| panic!(".env.example must document {name}"));
        assert_eq!(
            line.trim(),
            format!("{name}="),
            "{name} must ship blank; secrets come from a secret manager"
        );
    }
}

#[test]
fn adapter_debug_output_redacts_every_secret() {
    let kalshi = KalshiConfig {
        api_key: KALSHI_KEY_ID.into(),
        private_key_pem: "-----BEGIN RSA PRIVATE KEY-----\nMIIabc123def456ghi789==\n-----END RSA PRIVATE KEY-----\n".into(),
        base_url: "https://example.test".into(),
        timestamp_ms: None,
        request_timeout: None,
    };
    let rendered = format!("{kalshi:?}");
    assert_eq!(rendered.matches("[redacted]").count(), 2);
    assert!(!rendered.contains(KALSHI_KEY_ID));
    assert!(!rendered.contains("MIIabc123"));

    let poly = PolymarketConfig {
        wallet_address: "0xpublicwalletaddress0001".into(),
        l1_private_key: POLY_L1.into(),
        api_key: POLY_KEY.into(),
        api_secret: POLY_SECRET.into(),
        passphrase: POLY_PASSPHRASE.into(),
        base_url: "https://example.test".into(),
        timestamp_s: None,
        request_timeout: None,
    };
    let rendered = format!("{poly:?}");
    assert_eq!(rendered.matches("[redacted]").count(), 4);
    for secret in [POLY_L1, POLY_KEY, POLY_SECRET, POLY_PASSPHRASE] {
        assert!(!rendered.contains(secret));
    }
    // The wallet address is public by design and stays visible.
    assert!(rendered.contains("0xpublicwalletaddress0001"));
}

#[test]
fn scanner_catches_a_leaked_artifact_and_passes_a_clean_one() {
    // Negative control first: the scanner must be able to find material, or
    // its clean bill of health proves nothing.
    let scan = SecretScan::from_values([
        ("KALSHI_ACCESS_KEY_ID", KALSHI_KEY_ID),
        ("POLY_PRIVATE_KEY", POLY_L1),
        ("POLY_API_KEY", POLY_KEY),
        ("POLY_API_SECRET", POLY_SECRET),
        ("POLY_API_PASSPHRASE", POLY_PASSPHRASE),
    ]);
    let planted = format!("journal line quoting {POLY_SECRET} verbatim");
    assert_eq!(scan.scan_bytes(planted.as_bytes()), vec!["POLY_API_SECRET"]);

    // A faithful dry-run artifact shape stays clean.
    let frame = r#"{"t":"session-start","run_id":"prod-173","mode":"dry-run"}"#;
    assert_eq!(scan.scan_bytes(frame.as_bytes()), Vec::<&str>::new());
}

#[test]
fn file_sweep_names_path_and_label_but_never_the_value() {
    let dir = std::env::temp_dir().join(format!("arbkit-hygiene-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dirty = dir.join("prod-session.ndjson");
    std::fs::write(&dirty, format!("attempt record with {POLY_KEY} embedded")).unwrap();

    let scan = SecretScan::from_values([("POLY_API_KEY", POLY_KEY)]);
    let error = scan.assert_files_clean(&[&dirty]).unwrap_err();
    assert!(error.contains("prod-session.ndjson"));
    assert!(error.contains("POLY_API_KEY"));
    assert!(!error.contains(POLY_KEY));

    let _ = std::fs::remove_dir_all(&dir);
}
