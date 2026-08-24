//! Authenticated Polymarket CLOB execution adapter.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use hmac::{Hmac, Mac};
use k256::ecdsa::{signature::Signer, Signature, SigningKey};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{ExecLeg, FillEvent, OrderResult, VenueAdapter, VenueInstrumentRef};

type HmacSha256 = Hmac<Sha256>;

/// Wallet and L1/L2 credentials for the CLOB API.
#[derive(Clone)]
pub struct PolymarketConfig {
    /// Wallet address used as the maker/funder.
    pub wallet_address: String,
    /// L1 wallet private key material retained by the host signer.
    pub l1_private_key: String,
    /// CLOB API key.
    pub api_key: String,
    /// CLOB API secret used for L2 HMAC authentication.
    pub api_secret: String,
    /// CLOB passphrase.
    pub passphrase: String,
    /// API origin.
    pub base_url: String,
    /// Fixed timestamp for fixtures; production uses current seconds.
    pub timestamp_s: Option<u64>,
}

impl std::fmt::Debug for PolymarketConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolymarketConfig")
            .field("wallet_address", &self.wallet_address)
            .field("l1_private_key", &"[redacted]")
            .field("api_key", &"[redacted]")
            .field("api_secret", &"[redacted]")
            .field("passphrase", &"[redacted]")
            .field("base_url", &self.base_url)
            .finish()
    }
}

/// Polymarket adapter errors.
#[derive(Debug, thiserror::Error)]
pub enum PolymarketError {
    /// Missing credentials or unsupported instrument.
    #[error("invalid Polymarket configuration: {0}")]
    Configuration(String),
    /// HMAC or request construction failure.
    #[error("Polymarket signing failed: {0}")]
    Signing(String),
    /// Transport failure.
    #[error("Polymarket transport failed: {0}")]
    Transport(String),
    /// Non-success API response.
    #[error("Polymarket API rejected request ({status}): {message}")]
    Api {
        /// HTTP status.
        status: u16,
        /// Response body.
        message: String,
    },
    /// Invalid response payload.
    #[error("invalid Polymarket response: {0}")]
    Response(String),
}

/// Authenticated synchronous CLOB adapter.
pub struct PolymarketExecutionAdapter {
    config: PolymarketConfig,
    client: Client,
}

impl std::fmt::Debug for PolymarketExecutionAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolymarketExecutionAdapter")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl PolymarketExecutionAdapter {
    /// Validate credentials and construct an adapter.
    pub fn new(config: PolymarketConfig) -> Result<Self, PolymarketError> {
        if config.wallet_address.is_empty()
            || config.l1_private_key.is_empty()
            || config.api_key.is_empty()
            || config.api_secret.is_empty()
            || config.passphrase.is_empty()
        {
            return Err(PolymarketError::Configuration(
                "wallet, L1 key, API key, secret, and passphrase are required".into(),
            ));
        }
        let client = Client::builder()
            .build()
            .map_err(|e| PolymarketError::Transport(e.to_string()))?;
        Ok(Self { config, client })
    }
    /// Produce the L2 HMAC signature over timestamp, method, path, and body.
    pub fn sign(
        &self,
        timestamp_s: u64,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<String, PolymarketError> {
        let mut mac = HmacSha256::new_from_slice(self.config.api_secret.as_bytes())
            .map_err(|e| PolymarketError::Signing(e.to_string()))?;
        mac.update(format!("{timestamp_s}{method}{path}{body}").as_bytes());
        Ok(STANDARD.encode(mac.finalize().into_bytes()))
    }

    /// Sign an L1 wallet message with the configured secp256k1 key.
    pub fn sign_l1_message(&self, message: &[u8]) -> Result<String, PolymarketError> {
        let key_hex = self
            .config
            .l1_private_key
            .strip_prefix("0x")
            .unwrap_or(&self.config.l1_private_key);
        let bytes = hex_decode(key_hex)
            .ok_or_else(|| PolymarketError::Signing("L1 private key must be hex".into()))?;
        let key =
            SigningKey::from_slice(&bytes).map_err(|e| PolymarketError::Signing(e.to_string()))?;
        let signature: Signature = key.sign(message);
        Ok(format!("0x{}", hex_encode(signature.to_bytes().as_ref())))
    }
    fn timestamp_s(&self) -> u64 {
        self.config.timestamp_s.unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        })
    }
    fn request(
        &self,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<reqwest::blocking::RequestBuilder, PolymarketError> {
        let timestamp = self.timestamp_s();
        let signature = self.sign(timestamp, method, path, body)?;
        let url = format!("{}{}", self.config.base_url.trim_end_matches('/'), path);
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|e| PolymarketError::Configuration(e.to_string()))?;
        Ok(self
            .client
            .request(method, url)
            .header("POLY-ADDRESS", &self.config.wallet_address)
            .header("POLY-API-KEY", &self.config.api_key)
            .header("POLY-PASSPHRASE", &self.config.passphrase)
            .header("POLY-SIGNATURE", signature)
            .header("POLY-TIMESTAMP", timestamp.to_string())
            .header("Content-Type", "application/json")
            .body(body.to_owned()))
    }
    fn send<T: for<'de> Deserialize<'de>>(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> Result<T, PolymarketError> {
        let response = request
            .send()
            .map_err(|e| PolymarketError::Transport(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(PolymarketError::Api {
                status: status.as_u16(),
                message: response.text().unwrap_or_else(|_| "empty response".into()),
            });
        }
        response
            .json()
            .map_err(|e| PolymarketError::Response(e.to_string()))
    }
    /// Submit an authenticated FOK or IOC order.
    pub fn place_order(
        &self,
        leg: &ExecLeg,
        time_in_force: TimeInForce,
    ) -> Result<OrderResult, PolymarketError> {
        let VenueInstrumentRef::Polymarket(token) = &leg.instrument else {
            return Err(PolymarketError::Configuration(
                "Polymarket adapter received a non-Polymarket instrument".into(),
            ));
        };
        let token_id = String::from_utf8_lossy(token)
            .trim_matches(char::from(0))
            .to_string();
        let body = serde_json::to_string(&PolymarketOrderRequest {
            token_id,
            side: "BUY",
            price_ppm: leg.limit_price.ppm(),
            size_cents: leg.stake_cents,
            order_type: time_in_force.as_str(),
            client_order_id: hex_id(&leg.client_order_id),
        })
        .map_err(|e| PolymarketError::Response(e.to_string()))?;
        let response: OrderResponse = self.send(self.request("POST", "/order", &body)?)?;
        Ok(OrderResult {
            order_id: response.order_id,
            filled_stake_cents: response.filled_stake_cents.unwrap_or(0),
        })
    }
    /// Cancel an order.
    pub fn cancel_order(&self, order_id: &str) -> Result<(), PolymarketError> {
        let path = format!("/order/{order_id}");
        let _: serde_json::Value = self.send(self.request("DELETE", &path, "")?)?;
        Ok(())
    }
    /// Poll order status.
    pub fn order_status(&self, order_id: &str) -> Result<PolymarketOrderStatus, PolymarketError> {
        let path = format!("/order/{order_id}");
        self.send(self.request("GET", &path, "")?)
    }
    /// Return the account balance/allowance value in cents.
    pub fn balance_cents(&self) -> Result<i64, PolymarketError> {
        let response: BalanceResponse =
            self.send(self.request("GET", "/balance-allowance", "")?)?;
        Ok(response.balance_cents)
    }
}

impl VenueAdapter for PolymarketExecutionAdapter {
    fn submit(&self, leg: &ExecLeg) -> Result<OrderResult, String> {
        self.place_order(leg, TimeInForce::Fok)
            .map_err(|e| e.to_string())
    }
    fn unwind(&self, order: &OrderResult) -> Result<(), String> {
        self.cancel_order(&order.order_id)
            .map_err(|e| e.to_string())
    }
}

/// Supported immediate-or-cancel order policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TimeInForce {
    /// Fill entirely or reject.
    Fok,
    /// Fill available size and cancel remainder.
    Ioc,
}
impl TimeInForce {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fok => "FOK",
            Self::Ioc => "IOC",
        }
    }
}
#[derive(Debug, Serialize)]
struct PolymarketOrderRequest {
    token_id: String,
    side: &'static str,
    price_ppm: u32,
    size_cents: i64,
    order_type: &'static str,
    client_order_id: String,
}
#[derive(Debug, Deserialize)]
struct OrderResponse {
    order_id: String,
    filled_stake_cents: Option<i64>,
}
/// Authoritative order state.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PolymarketOrderStatus {
    /// Venue order ID.
    pub order_id: String,
    /// Venue lifecycle status.
    pub status: Option<String>,
    /// Filled stake.
    pub filled_stake_cents: Option<i64>,
}
#[derive(Debug, Deserialize)]
struct BalanceResponse {
    balance_cents: i64,
}
fn hex_id(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn hex_decode(raw: &str) -> Option<Vec<u8>> {
    if raw.len() % 2 != 0 {
        return None;
    }
    (0..raw.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&raw[i..i + 2], 16).ok())
        .collect()
}

/// Decode a private Polymarket fill frame for reconciliation.
pub fn parse_fill_event(raw_json: &str) -> Result<FillEvent, PolymarketError> {
    #[derive(Deserialize)]
    struct Frame {
        #[serde(alias = "orderId")]
        order_id: String,
        #[serde(alias = "clientOrderId")]
        client_order_id: Option<String>,
        #[serde(alias = "filledStakeCents")]
        filled_stake_cents: i64,
        #[serde(alias = "feeCents")]
        fee_cents: Option<i64>,
        #[serde(alias = "realizedProfitCents")]
        realized_profit_cents: Option<i64>,
        status: Option<String>,
    }
    let frame: Frame =
        serde_json::from_str(raw_json).map_err(|e| PolymarketError::Response(e.to_string()))?;
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
    use crate::VenueInstrumentRef;
    use arbkit_core::Prob;
    #[test]
    fn l2_signature_and_order_policy_are_deterministic() {
        let adapter = PolymarketExecutionAdapter::new(PolymarketConfig {
            wallet_address: "0xabc".into(),
            l1_private_key: "wallet-key".into(),
            api_key: "key".into(),
            api_secret: "secret".into(),
            passphrase: "pass".into(),
            base_url: "http://localhost".into(),
            timestamp_s: Some(10),
        })
        .unwrap();
        let signature = adapter.sign(10, "POST", "/order", "{}").unwrap();
        assert_eq!(STANDARD.decode(signature).unwrap().len(), 32);
        assert_eq!(TimeInForce::Fok.as_str(), "FOK");
        assert_eq!(TimeInForce::Ioc.as_str(), "IOC");
    }

    #[test]
    fn l1_wallet_message_signing_requires_hex_key() {
        let adapter = PolymarketExecutionAdapter::new(PolymarketConfig {
            wallet_address: "0xabc".into(),
            l1_private_key: "0x0101010101010101010101010101010101010101010101010101010101010101"
                .into(),
            api_key: "key".into(),
            api_secret: "secret".into(),
            passphrase: "pass".into(),
            base_url: "http://localhost".into(),
            timestamp_s: Some(10),
        })
        .unwrap();
        assert!(adapter
            .sign_l1_message(b"clob-auth")
            .unwrap()
            .starts_with("0x"));
    }

    #[test]
    fn private_fill_frame_decodes_for_reconciliation() {
        let fill = parse_fill_event(r#"{"orderId":"p1","clientOrderId":"02020202020202020202020202020202","filledStakeCents":70,"feeCents":1,"status":"open"}"#).unwrap();
        assert_eq!(fill.client_order_id, Some([2; 16]));
        assert_eq!(fill.filled_stake_cents, 70);
    }

    #[test]
    fn http_fixtures_cover_order_status_and_balance() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let size = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..size]);
                let body = if request.starts_with("POST /order") {
                    r#"{"order_id":"poly-1","filled_stake_cents":100}"#
                } else if request.starts_with("GET /order/poly-1") {
                    r#"{"order_id":"poly-1","status":"matched","filled_stake_cents":100}"#
                } else {
                    r#"{"balance_cents":999}"#
                };
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let adapter = PolymarketExecutionAdapter::new(PolymarketConfig {
            wallet_address: "0xabc".into(),
            l1_private_key: "wallet-key".into(),
            api_key: "key".into(),
            api_secret: "secret".into(),
            passphrase: "pass".into(),
            base_url: format!("http://{address}"),
            timestamp_s: Some(10),
        })
        .unwrap();
        let leg = ExecLeg {
            venue: 2,
            instrument: VenueInstrumentRef::Polymarket([b't'; 32]),
            limit_price: Prob::from_cents(50).unwrap(),
            stake_cents: 100,
            client_order_id: [2; 16],
        };
        assert_eq!(
            adapter
                .place_order(&leg, TimeInForce::Fok)
                .unwrap()
                .order_id,
            "poly-1"
        );
        assert_eq!(
            adapter.order_status("poly-1").unwrap().status.as_deref(),
            Some("matched")
        );
        assert_eq!(adapter.balance_cents().unwrap(), 999);
        server.join().unwrap();
    }
}
