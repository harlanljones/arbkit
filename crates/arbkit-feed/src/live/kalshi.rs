//! Kalshi WebSocket market data connector.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async, tungstenite::client::IntoClientRequest, tungstenite::http, tungstenite::Message,
    MaybeTlsStream, WebSocketStream,
};

use crate::event::FeedEvent;
use crate::live::bridge::FeedEventSender;
use crate::venues::{KalshiParser, VENUE_KALSHI};
use arbkit_core::{MarketId, OutcomeId};

/// One Kalshi ticker mapped to canonical ids.
#[derive(Debug, Clone)]
pub struct KalshiSubscription {
    /// Kalshi market ticker (e.g. `KXNBAGAME-26AUG181930BOSLAL-BOS`).
    pub ticker: String,
    /// Canonical market id.
    pub market_id: MarketId,
    /// Canonical YES / team outcome id.
    pub yes_outcome_id: OutcomeId,
    /// Canonical NO / opposing outcome id.
    pub no_outcome_id: OutcomeId,
}

/// Configuration for a Kalshi live feed session.
#[derive(Debug, Clone)]
pub struct KalshiFeedConfig {
    /// WebSocket URL (production or demo).
    pub ws_url: String,
    /// Markets to subscribe to.
    pub subscriptions: Vec<KalshiSubscription>,
    /// Kalshi API key id. Empty attempts an anonymous connect, which the
    /// venue rejects; market data requires authentication.
    pub api_key: String,
    /// PEM-encoded RSA private key matching `api_key`, used to sign the
    /// connect handshake exactly like the REST adapter signs requests.
    pub private_key_pem: String,
}

impl Default for KalshiFeedConfig {
    fn default() -> Self {
        Self {
            ws_url: "wss://api.elections.kalshi.com/trade-api/ws/v2".to_string(),
            subscriptions: Vec::new(),
            api_key: String::new(),
            private_key_pem: String::new(),
        }
    }
}

impl KalshiFeedConfig {
    fn signer(&self) -> Option<WsSigner> {
        if self.api_key.is_empty() || self.private_key_pem.is_empty() {
            return None;
        }
        WsSigner::new(&self.api_key, &self.private_key_pem).ok()
    }
}

/// RSA-PSS handshake signer for Kalshi's authenticated market-data socket.
struct WsSigner {
    api_key: String,
    key: rsa::RsaPrivateKey,
}

impl WsSigner {
    fn new(api_key: &str, pem: &str) -> Result<Self, String> {
        use rsa::pkcs8::DecodePrivateKey;
        let key = rsa::RsaPrivateKey::from_pkcs8_pem(pem)
            .map_err(|e| format!("parse KALSHI_PRIVATE_KEY_PATH contents: {e}"))?;
        Ok(Self {
            api_key: api_key.to_string(),
            key,
        })
    }

    fn timestamp_ms(&self) -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }

    fn headers(&self) -> Result<[(String, String); 3], String> {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use rsa::signature::{RandomizedSigner, SignatureEncoding};
        let ts = self.timestamp_ms();
        let preimage = format!("{ts}GET/trade-api/ws/v2");
        let signing = rsa::pss::SigningKey::<sha2::Sha256>::new(self.key.clone());
        let signature = signing.sign_with_rng(&mut rand::thread_rng(), preimage.as_bytes());
        Ok([
            ("KALSHI-ACCESS-KEY".to_string(), self.api_key.clone()),
            (
                "KALSHI-ACCESS-SIGNATURE".to_string(),
                STANDARD.encode(signature.to_bytes()),
            ),
            ("KALSHI-ACCESS-TIMESTAMP".to_string(), ts.to_string()),
        ])
    }
}

/// Async Kalshi feed task controller.
pub struct KalshiLiveFeed;

impl KalshiLiveFeed {
    /// Run until `running` is cleared or the connection fails permanently.
    pub async fn run(
        config: KalshiFeedConfig,
        sender: impl FeedEventSender,
        running: Arc<AtomicBool>,
    ) {
        while running.load(Ordering::Relaxed) {
            match Self::session(&config, &sender, &running).await {
                Ok(()) => break,
                Err(error) => {
                    eprintln!("kalshi feed error: {error}; reconnecting in 2s");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }

    async fn session(
        config: &KalshiFeedConfig,
        sender: &impl FeedEventSender,
        running: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        // Kalshi's market-data socket sits behind the same RSA-PSS auth as
        // REST: sign the handshake when credentials are supplied, else
        // connect bare and let the venue's 401 explain itself.
        let mut ws = match config.signer() {
            Some(signer) => {
                let mut request = config
                    .ws_url
                    .as_str()
                    .into_client_request()
                    .map_err(|e| format!("request: {e}"))?;
                for (name, value) in signer.headers()? {
                    let header_name = http::HeaderName::try_from(name.as_str())
                        .map_err(|e| format!("{name}: {e}"))?;
                    let header_value =
                        http::HeaderValue::from_str(&value).map_err(|e| format!("{name}: {e}"))?;
                    request.headers_mut().insert(header_name, header_value);
                }
                let (stream, _) = connect_async(request)
                    .await
                    .map_err(|e| format!("connect: {e}"))?;
                stream
            }
            None => {
                let (stream, _) = connect_async(&config.ws_url)
                    .await
                    .map_err(|e| format!("connect: {e}"))?;
                stream
            }
        };

        for (subscription_id, sub) in config.subscriptions.iter().enumerate() {
            let subscribe = serde_json::json!({
                "id": subscription_id as u64 + 1,
                "cmd": "subscribe",
                "params": {
                    "channels": ["orderbook_delta"],
                    "market_ticker": sub.ticker,
                }
            });
            ws.send(Message::Text(subscribe.to_string().into()))
                .await
                .map_err(|e| format!("subscribe: {e}"))?;
        }

        let mut parser = KalshiParser::new();
        let ticker_map: std::collections::HashMap<String, (MarketId, OutcomeId, OutcomeId)> =
            config
                .subscriptions
                .iter()
                .map(|s| {
                    (
                        s.ticker.clone(),
                        (s.market_id, s.yes_outcome_id, s.no_outcome_id),
                    )
                })
                .collect();

        while running.load(Ordering::Relaxed) {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(30), ws.next())
                .await
                .map_err(|_| "read timeout".to_string())?;

            let Some(msg) = msg else {
                return Err("socket closed".to_string());
            };

            let text = match msg.map_err(|e| format!("ws: {e}"))? {
                Message::Text(t) => t.to_string(),
                Message::Ping(p) => {
                    let _ = ws.send(Message::Pong(p)).await;
                    continue;
                }
                Message::Close(_) => return Err("server closed".to_string()),
                _ => continue,
            };

            let ticker = extract_kalshi_ticker(&text).unwrap_or_default();
            let Some((market_id, yes_id, no_id)) = ticker_map.get(&ticker).copied() else {
                // Broadcast frames may omit ticker; try each subscription.
                let mut pushed = false;
                for (market_id, yes_id, no_id) in ticker_map.values().copied() {
                    if let Ok(feed_msg) =
                        parser.parse_json(&text, market_id, yes_id, no_id, now_ns())
                    {
                        for event in feed_msg.events() {
                            if sender.send(*event) {
                                pushed = true;
                            }
                        }
                    }
                }
                if !pushed {
                    continue;
                }
                continue;
            };

            match parser.parse_json(&text, market_id, yes_id, no_id, now_ns()) {
                Ok(feed_msg) => {
                    for event in feed_msg.events() {
                        if !sender.send(*event) {
                            return Ok(());
                        }
                    }
                }
                Err(crate::error::FeedError::SequenceGap { .. }) => {
                    let _ =
                        sender.send(FeedEvent::halt(VENUE_KALSHI, market_id, None, now_ns(), 1));
                }
                Err(_) => {}
            }
        }

        Ok(())
    }
}

fn extract_kalshi_ticker(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value
        .get("msg")
        .and_then(|m| m.get("market_ticker"))
        .or_else(|| value.get("market_ticker"))
        .and_then(|t| t.as_str())
        .map(str::to_string)
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[allow(dead_code)]
type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[cfg(all(test, feature = "live"))]
mod tests {
    use super::*;

    /// The handshake signer must emit exactly the three Kalshi auth headers
    /// with a signature that verifies against the supplied public key —
    /// same preimage shape as the REST adapter (`ts + GET + path`).
    #[test]
    fn ws_signer_produces_verifiable_auth_headers() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use rsa::pkcs8::EncodePrivateKey;
        use rsa::signature::Verifier;
        use rsa::{pss::VerifyingKey, RsaPrivateKey};

        let mut rng = rand::thread_rng();
        let key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pem = key.to_pkcs8_pem(Default::default()).unwrap().to_string();

        let signer = WsSigner::new("test-key-id-0001", &pem).unwrap();
        let headers = signer.headers().unwrap();

        assert_eq!(headers[0].0, "KALSHI-ACCESS-KEY");
        assert_eq!(headers[0].1, "test-key-id-0001");
        assert_eq!(headers[2].0, "KALSHI-ACCESS-TIMESTAMP");

        // Reconstruct the preimage from the emitted timestamp and verify.
        let ts = &headers[2].1;
        let sig = STANDARD.decode(&headers[1].1).unwrap();
        let preimage = format!("{ts}GET/trade-api/ws/v2");
        let verifying = VerifyingKey::<sha2::Sha256>::new(rsa::RsaPublicKey::from(&key));
        verifying
            .verify(
                preimage.as_bytes(),
                &rsa::pss::Signature::try_from(sig.as_slice()).unwrap(),
            )
            .expect("signature verifies over the documented preimage");
    }
}
