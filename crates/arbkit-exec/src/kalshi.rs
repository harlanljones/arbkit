//! Authenticated Kalshi REST execution adapter.
//!
//! The adapter is deliberately synchronous because [`crate::VenueAdapter`] is
//! an application-boundary trait. Network I/O never enters the detector hot
//! path. Credentials are supplied by the caller and are not logged.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::blocking::Client;
use rsa::signature::{RandomizedSigner, SignatureEncoding};
use rsa::{pkcs8::DecodePrivateKey, pss::SigningKey, RsaPrivateKey};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{ExecLeg, FillEvent, OrderResult, VenueAdapter, VenueInstrumentRef};

/// Credentials and endpoint configuration for Kalshi's trade API.
#[derive(Clone)]
pub struct KalshiConfig {
    /// API key identifier sent in `KALSHI-ACCESS-KEY`.
    pub api_key: String,
    /// PEM-encoded RSA private key used for RSA-PSS/SHA-256 signing.
    pub private_key_pem: String,
    /// API origin, normally `https://api.elections.kalshi.com`.
    pub base_url: String,
    /// Optional clock source override, useful for deterministic tests.
    pub timestamp_ms: Option<u64>,
    /// Per-request deadline; a venue that cannot answer inside it fails the
    /// hedge leg like any other rejection instead of stalling the runner.
    pub request_timeout: Option<std::time::Duration>,
}

impl std::fmt::Debug for KalshiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KalshiConfig")
            .field("api_key", &"[redacted]")
            .field("private_key_pem", &"[redacted]")
            .field("base_url", &self.base_url)
            .field("timestamp_ms", &self.timestamp_ms)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl Default for KalshiConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            private_key_pem: String::new(),
            base_url: "https://api.elections.kalshi.com".into(),
            timestamp_ms: None,
            request_timeout: Some(std::time::Duration::from_secs(5)),
        }
    }
}

impl KalshiConfig {
    /// Configuration preset targeting Kalshi's demo environment. The URL can
    /// be overridden afterwards; no network claim is made by constructing it.
    pub fn demo(api_key: String, private_key_pem: String) -> Self {
        Self {
            api_key,
            private_key_pem,
            base_url: "https://demo-api.kalshi.co".into(),
            ..Self::default()
        }
    }
}

/// Errors returned by the authenticated adapter.
#[derive(Debug, thiserror::Error)]
pub enum KalshiError {
    /// Credentials or an instrument were invalid before making a request.
    #[error("invalid Kalshi configuration: {0}")]
    Configuration(String),
    /// Request signing failed.
    #[error("Kalshi signing failed: {0}")]
    Signing(String),
    /// Transport-level failure.
    #[error("Kalshi transport failed: {0}")]
    Transport(String),
    /// Kalshi returned a non-success response.
    #[error("Kalshi API rejected request ({status}): {message}")]
    Api {
        /// HTTP status returned by Kalshi.
        status: u16,
        /// Response body or venue error message.
        message: String,
    },
    /// Response JSON did not match the expected contract.
    #[error("invalid Kalshi response: {0}")]
    Response(String),
}

/// A signed, authenticated Kalshi REST client implementing order execution.
pub struct KalshiExecutionAdapter {
    config: KalshiConfig,
    key: RsaPrivateKey,
    client: Client,
}

impl std::fmt::Debug for KalshiExecutionAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KalshiExecutionAdapter")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl KalshiExecutionAdapter {
    /// Construct an adapter, parsing and validating the RSA private key once.
    pub fn new(config: KalshiConfig) -> Result<Self, KalshiError> {
        if config.api_key.is_empty() || config.private_key_pem.is_empty() {
            return Err(KalshiError::Configuration(
                "API key and private key are required".into(),
            ));
        }
        let key = RsaPrivateKey::from_pkcs8_pem(&config.private_key_pem)
            .map_err(|e| KalshiError::Configuration(format!("private key: {e}")))?;
        let mut builder = Client::builder();
        if let Some(timeout) = config.request_timeout {
            builder = builder.timeout(timeout);
        }
        let client = builder
            .build()
            .map_err(|e| KalshiError::Transport(e.to_string()))?;
        Ok(Self {
            config,
            key,
            client,
        })
    }

    /// Return a signature for the Kalshi signing preimage.
    pub fn sign(&self, timestamp_ms: u64, method: &str, path: &str) -> Result<String, KalshiError> {
        let preimage = format!("{timestamp_ms}{method}{path}");
        let signing_key = SigningKey::<Sha256>::new(self.key.clone());
        let signature = signing_key.sign_with_rng(&mut rand::thread_rng(), preimage.as_bytes());
        Ok(STANDARD.encode(signature.to_bytes()))
    }

    fn timestamp_ms(&self) -> u64 {
        self.config.timestamp_ms.unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        })
    }

    fn request(
        &self,
        method: &str,
        path: &str,
    ) -> Result<reqwest::blocking::RequestBuilder, KalshiError> {
        let timestamp = self.timestamp_ms();
        let signature = self.sign(timestamp, method, path)?;
        let url = format!("{}{}", self.config.base_url.trim_end_matches('/'), path);
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|e| KalshiError::Configuration(format!("method: {e}")))?;
        Ok(self
            .client
            .request(method, url)
            .header("KALSHI-ACCESS-KEY", &self.config.api_key)
            .header("KALSHI-ACCESS-SIGNATURE", signature)
            .header("KALSHI-ACCESS-TIMESTAMP", timestamp.to_string()))
    }

    fn send<T: for<'de> Deserialize<'de>>(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> Result<T, KalshiError> {
        let response = request
            .send()
            .map_err(|e| KalshiError::Transport(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let message = response.text().unwrap_or_else(|_| "empty response".into());
            return Err(KalshiError::Api {
                status: status.as_u16(),
                message,
            });
        }
        response
            .json()
            .map_err(|e| KalshiError::Response(e.to_string()))
    }

    /// Place a FOK limit order and require the response to identify it.
    pub fn place_order(&self, leg: &ExecLeg) -> Result<OrderResult, KalshiError> {
        let VenueInstrumentRef::Kalshi(ticker) = &leg.instrument else {
            return Err(KalshiError::Configuration(
                "Kalshi adapter received a non-Kalshi instrument".into(),
            ));
        };
        let path = "/trade-api/v2/portfolio/orders";
        let body = KalshiOrderRequest {
            ticker,
            client_order_id: hex_id(&leg.client_order_id),
            side: "yes",
            action: "buy",
            count: leg.stake_cents,
            yes_price: leg.limit_price.ppm() / 10_000,
            time_in_force: "fill_or_kill",
        };
        let response: KalshiOrderResponse = self.send(self.request("POST", path)?.json(&body))?;
        Ok(OrderResult {
            order_id: response.order.order_id,
            filled_stake_cents: response.order.filled_count.unwrap_or(0),
        })
    }

    /// Cancel an accepted order, used by hedge unwind.
    pub fn cancel_order(&self, order_id: &str) -> Result<(), KalshiError> {
        let path = format!("/trade-api/v2/portfolio/orders/{order_id}");
        let _: serde_json::Value = self.send(self.request("DELETE", &path)?)?;
        Ok(())
    }

    /// Poll the authoritative order state.
    pub fn order_status(&self, order_id: &str) -> Result<OrderStatus, KalshiError> {
        let path = format!("/trade-api/v2/portfolio/orders/{order_id}");
        let response: KalshiOrderResponse = self.send(self.request("GET", &path)?)?;
        Ok(response.order)
    }

    /// Read the account balance in cents.
    pub fn balance_cents(&self) -> Result<i64, KalshiError> {
        let response: BalanceResponse =
            self.send(self.request("GET", "/trade-api/v2/portfolio/balance")?)?;
        Ok(response.balance)
    }
}

impl VenueAdapter for KalshiExecutionAdapter {
    fn submit(&self, leg: &ExecLeg) -> Result<OrderResult, String> {
        self.place_order(leg).map_err(|e| e.to_string())
    }

    fn unwind(&self, order: &OrderResult) -> Result<(), String> {
        self.cancel_order(&order.order_id)
            .map_err(|e| e.to_string())
    }
}

/// Authoritative status returned by Kalshi's order endpoint.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct OrderStatus {
    /// Venue order identifier.
    pub order_id: String,
    /// Filled contract count, if supplied.
    pub filled_count: Option<i64>,
    /// Current order status.
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
struct KalshiOrderRequest<'a> {
    ticker: &'a str,
    client_order_id: String,
    side: &'static str,
    action: &'static str,
    count: i64,
    yes_price: u32,
    time_in_force: &'static str,
}

#[derive(Debug, Deserialize)]
struct KalshiOrderResponse {
    order: OrderStatus,
}

#[derive(Debug, Deserialize)]
struct BalanceResponse {
    balance: i64,
}

fn hex_id(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode a private Kalshi fill frame for reconciliation.
pub fn parse_fill_event(raw_json: &str) -> Result<FillEvent, KalshiError> {
    #[derive(Deserialize)]
    struct Frame {
        order_id: String,
        client_order_id: Option<String>,
        filled_stake_cents: i64,
        fee_cents: Option<i64>,
        realized_profit_cents: Option<i64>,
        status: Option<String>,
    }
    let frame: Frame =
        serde_json::from_str(raw_json).map_err(|e| KalshiError::Response(e.to_string()))?;
    Ok(FillEvent {
        client_order_id: frame.client_order_id.as_deref().and_then(decode_id),
        venue_order_id: frame.order_id,
        filled_stake_cents: frame.filled_stake_cents,
        fee_cents: frame.fee_cents.unwrap_or(0),
        realized_profit_cents: frame.realized_profit_cents,
        status: frame.status.unwrap_or_else(|| "open".into()),
    })
}
fn decode_id(raw: &str) -> Option<[u8; 16]> {
    if raw.len() != 32 {
        return None;
    }
    let mut id = [0; 16];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pss::VerifyingKey;
    use rsa::signature::Verifier;
    use rsa::{pkcs8::EncodePrivateKey, RsaPrivateKey, RsaPublicKey};

    fn adapter() -> KalshiExecutionAdapter {
        adapter_with_base("http://127.0.0.1:1")
    }

    fn adapter_with_base(base_url: &str) -> KalshiExecutionAdapter {
        let key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let pem = key.to_pkcs8_pem(Default::default()).unwrap().to_string();
        KalshiExecutionAdapter::new(KalshiConfig {
            api_key: "demo-key".into(),
            private_key_pem: pem,
            base_url: base_url.into(),
            timestamp_ms: Some(1_700_000_000_000),
            request_timeout: None,
        })
        .unwrap()
    }

    #[test]
    fn signing_round_trip() {
        let adapter = adapter();
        let encoded = adapter
            .sign(1_700_000_000_000, "POST", "/trade-api/v2/portfolio/orders")
            .unwrap();
        let signature = STANDARD.decode(encoded).unwrap();
        let public = RsaPublicKey::from(&adapter.key);
        let verifying_key = VerifyingKey::<Sha256>::new(public);
        let signature = rsa::pss::Signature::try_from(signature.as_slice()).unwrap();
        verifying_key
            .verify(
                b"1700000000000POST/trade-api/v2/portfolio/orders",
                &signature,
            )
            .unwrap();
    }

    #[test]
    fn signing_rejects_missing_credentials() {
        let result = KalshiExecutionAdapter::new(KalshiConfig::default());
        assert!(matches!(result, Err(KalshiError::Configuration(_))));
    }

    #[test]
    fn request_body_uses_fok_and_idempotency_key() {
        let body = KalshiOrderRequest {
            ticker: "KXTEST",
            client_order_id: "0011".into(),
            side: "yes",
            action: "buy",
            count: 10,
            yes_price: 55,
            time_in_force: "fill_or_kill",
        };
        let json = serde_json::to_value(body).unwrap();
        assert_eq!(json["time_in_force"], "fill_or_kill");
        assert_eq!(json["client_order_id"], "0011");
    }

    #[test]
    fn private_fill_frame_decodes_for_reconciliation() {
        let fill = parse_fill_event(r#"{"order_id":"v1","client_order_id":"04040404040404040404040404040404","filled_stake_cents":80,"fee_cents":2,"status":"settled"}"#).unwrap();
        assert_eq!(fill.client_order_id, Some([4; 16]));
        assert_eq!(fill.status, "settled");
    }

    #[test]
    fn http_fixtures() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 8192];
                let size = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..size]).to_string();
                seen.push(request.clone());
                let (status, body) = if request.starts_with("POST /trade-api/v2/portfolio/orders")
                    || request.starts_with("GET /trade-api/v2/portfolio/orders/order-1")
                {
                    (
                        "200 OK",
                        r#"{"order":{"order_id":"order-1","filled_count":100,"status":"executed"}}"#,
                    )
                } else if request.starts_with("DELETE /trade-api/v2/portfolio/orders/order-1") {
                    ("200 OK", r#"{}"#)
                } else {
                    ("200 OK", r#"{"balance":12345}"#)
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
            seen
        });

        let adapter = adapter_with_base(&format!("http://{address}"));
        let leg = ExecLeg {
            venue: 1,
            instrument: VenueInstrumentRef::Kalshi("KXTEST".into()),
            limit_price: arbkit_core::Prob::from_cents(55).unwrap(),
            stake_cents: 100,
            client_order_id: [7; 16],
        };
        let placed = adapter.place_order(&leg).unwrap();
        assert_eq!(placed.order_id, "order-1");
        assert_eq!(placed.filled_stake_cents, 100);
        assert_eq!(
            adapter.order_status("order-1").unwrap().status.as_deref(),
            Some("executed")
        );
        adapter.cancel_order("order-1").unwrap();
        assert_eq!(adapter.balance_cents().unwrap(), 12345);

        let requests = server.join().unwrap();
        assert!(requests.iter().all(|request| request
            .to_ascii_lowercase()
            .contains("kalshi-access-key: demo-key")));
        assert!(requests[0].contains("time_in_force"));
        assert!(requests[0].contains("client_order_id"));
    }

    #[test]
    fn http_rejects_authentication_failure() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            // A client under parallel load may open more than one connection
            // (retries, speculation), so answer every one. Each request is
            // drained first: writing the response before the client finished
            // sending lets an early close surface as a transport error
            // instead of the intended status. Detached: the request below is
            // synchronous, and joining an endless accept loop would hang the
            // suite.
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let response =
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let mut buf = [0u8; 4096];
                if std::io::Read::read(&mut stream, &mut buf).is_err() {
                    break;
                }
                if stream.write_all(response.as_bytes()).is_err() {
                    break;
                }
            }
        });

        let adapter = adapter_with_base(&format!("http://{address}"));
        let error = adapter.balance_cents().unwrap_err();
        assert!(matches!(error, KalshiError::Api { status: 401, .. }));
        drop(server);
    }
}
