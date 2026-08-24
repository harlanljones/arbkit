//! Live WebSocket feed connectors (I/O boundary).
//!
//! Tokio runs here only. Parsed [`crate::FeedEvent`] values cross into the engine via
//! [`FeedEventSender`]; the hot loop never awaits.

#[cfg(feature = "live")]
mod bridge;
#[cfg(feature = "live")]
mod discovery;
#[cfg(feature = "live")]
mod kalshi;
#[cfg(feature = "live")]
mod polymarket;

#[cfg(feature = "live")]
pub use bridge::{
    crossbeam_bridge, spawn_ring_bridge, FeedEventReceiver, FeedEventSender, MpscFeedBridge,
};
#[cfg(feature = "live")]
pub use discovery::{
    build_catalog_generation, discover_kalshi_markets, discover_polymarket_propositions,
    refresh_catalog, CatalogBuildReport, CatalogGeneration, CatalogService, DiscoveredKalshiMarket,
    DiscoveredPolymarketProposition, DiscoveryError, DiscoveryStats, KalshiDiscoveryConfig,
    PolymarketDiscoveryConfig, RestDiscoverySource, KALSHI_MARKETS_URL,
    POLYMARKET_GAMMA_EVENTS_URL,
};
#[cfg(feature = "live")]
pub use kalshi::{KalshiFeedConfig, KalshiLiveFeed, KalshiSubscription};
#[cfg(feature = "live")]
pub use polymarket::{PolymarketFeedConfig, PolymarketLiveFeed, PolymarketSubscription};
