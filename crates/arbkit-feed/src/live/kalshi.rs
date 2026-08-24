//! Kalshi WebSocket market data connector.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use crate::event::FeedEvent;
use crate::live::bridge::FeedEventSender;
use crate::venues::{KalshiParser, VENUE_KALSHI};
use arbkit_core::{MarketId, OutcomeId};

/// One Kalshi ticker mapped to canonical ids.
#[derive(Debug, Clone)]
pub struct KalshiSubscription {
    /// Kalshi market ticker (e.g. `KXNBAGAME-26AUG18BOSLAL-BOS`).
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
}

impl Default for KalshiFeedConfig {
    fn default() -> Self {
        Self {
            ws_url: "wss://api.elections.kalshi.com/trade-api/ws/v2".to_string(),
            subscriptions: Vec::new(),
        }
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
        let (mut ws, _) = connect_async(&config.ws_url)
            .await
            .map_err(|e| format!("connect: {e}"))?;

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
