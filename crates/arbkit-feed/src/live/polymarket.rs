//! Polymarket CLOB WebSocket market channel connector.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::event::FeedEvent;
use crate::live::bridge::FeedEventSender;
use crate::venues::{PolymarketParser, VENUE_POLYMARKET};
use arbkit_core::{MarketId, OutcomeId};

/// One Polymarket token mapped to canonical ids.
#[derive(Debug, Clone)]
pub struct PolymarketSubscription {
    /// CLOB asset / token id (decimal string).
    pub token_id: String,
    /// Canonical market id.
    pub market_id: MarketId,
    /// Canonical outcome id for this token.
    pub outcome_id: OutcomeId,
}

/// Configuration for a Polymarket live feed session.
#[derive(Debug, Clone)]
pub struct PolymarketFeedConfig {
    /// WebSocket URL for the market channel.
    pub ws_url: String,
    /// Token subscriptions.
    pub subscriptions: Vec<PolymarketSubscription>,
}

impl Default for PolymarketFeedConfig {
    fn default() -> Self {
        Self {
            ws_url: "wss://ws-subscriptions-clob.polymarket.com/ws/market".to_string(),
            subscriptions: Vec::new(),
        }
    }
}

/// Async Polymarket feed task controller.
pub struct PolymarketLiveFeed;

impl PolymarketLiveFeed {
    /// Run until `running` is cleared or the connection fails permanently.
    pub async fn run(
        config: PolymarketFeedConfig,
        sender: impl FeedEventSender,
        running: Arc<AtomicBool>,
    ) {
        while running.load(Ordering::Relaxed) {
            match Self::session(&config, &sender, &running).await {
                Ok(()) => break,
                Err(error) => {
                    eprintln!("polymarket feed error: {error}; reconnecting in 2s");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }

    async fn session(
        config: &PolymarketFeedConfig,
        sender: &impl FeedEventSender,
        running: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        let (mut ws, _) = connect_async(&config.ws_url)
            .await
            .map_err(|e| format!("connect: {e}"))?;

        let asset_ids: Vec<&str> = config
            .subscriptions
            .iter()
            .map(|s| s.token_id.as_str())
            .collect();

        let subscribe = serde_json::json!({
            "assets_ids": asset_ids,
            "type": "market",
        });
        ws.send(Message::Text(subscribe.to_string().into()))
            .await
            .map_err(|e| format!("subscribe: {e}"))?;

        let token_map: std::collections::HashMap<String, (MarketId, OutcomeId)> = config
            .subscriptions
            .iter()
            .map(|s| (s.token_id.clone(), (s.market_id, s.outcome_id)))
            .collect();

        let mut parser = PolymarketParser::new();

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

            // Polymarket also uses a plain-text application heartbeat rather
            // than a WebSocket control frame. Reply before attempting JSON
            // decoding; otherwise the connection is eventually reaped as
            // idle despite receiving a valid keepalive.
            if text == "PING" {
                ws.send(Message::Text("PONG".to_string().into()))
                    .await
                    .map_err(|e| format!("heartbeat: {e}"))?;
                continue;
            }
            if text == "PONG" {
                let _ = sender.send(FeedEvent::heartbeat(VENUE_POLYMARKET, now_ns()));
                continue;
            }

            let token_id = extract_poly_token(&text).unwrap_or_default();
            let lookup = if token_id.is_empty() {
                token_map.values().copied().next()
            } else {
                token_map.get(&token_id).copied()
            };

            let Some((market_id, outcome_id)) = lookup else {
                continue;
            };

            match parser.parse_json(&text, market_id, outcome_id, now_ns()) {
                Ok(feed_msg) => {
                    for event in feed_msg.events() {
                        if !sender.send(*event) {
                            return Ok(());
                        }
                    }
                }
                Err(crate::error::FeedError::SequenceGap { .. }) => {
                    let _ = sender.send(FeedEvent::halt(
                        VENUE_POLYMARKET,
                        market_id,
                        Some(outcome_id),
                        now_ns(),
                        1,
                    ));
                }
                Err(_) => {}
            }
        }

        Ok(())
    }
}

fn extract_poly_token(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value
        .get("asset_id")
        .or_else(|| value.get("token_id"))
        .and_then(|t| t.as_str())
        .map(str::to_string)
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
