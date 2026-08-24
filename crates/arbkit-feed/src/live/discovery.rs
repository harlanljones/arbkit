//! Live REST market discovery and catalog refresh (I/O boundary).
//!
//! Discovery fetches open markets from Kalshi and Polymarket over their public
//! REST APIs, normalizes each into a canonical proposition, pairs only what
//! survives strict validation, and installs the result as an immutable
//! [`CatalogGeneration`]. A refresh never mutates a snapshot a consumer may be
//! reading; it builds the next generation off to the side and swaps one `Arc`.
//!
//! Safety rules carried over from `LIVE_TRADING.md`:
//!
//! - Unmatched or ambiguous markets are omitted from the active map. They are
//!   counted in [`DiscoveryStats`], never guessed.
//! - A Polymarket proposition binds each CLOB token to a club through its own
//!   outcome label (`outcomes[i]` rides beside `clobTokenIds[i]`, and full
//!   team names resolve uniquely). The title must name exactly those two
//!   clubs — in `Away @ Home`, `at`, or `vs.` form — so a division race or
//!   award market can never masquerade as the game. Title order carries no
//!   home/away authority: sides are assigned at pairing time from the Kalshi
//!   fixture's own reading.
//! - Team labels resolve through [`lookup_team_unique`]; an alias shared by
//!   several teams across sports can never silently attach to the wrong sport.
//! - A proposition pairs only when exactly one open Kalshi moneyline event
//!   shares its clubs and date; same-site series rematches stay unpaired.
//! - Pairing still goes through [`VenueInstrumentMap::register_binary_pair`],
//!   so the existing complementarity gate is the last word.
//!
//! The HTTP transport is intentionally thin: page parsing is a pure function
//! of the response body, which keeps every non-trivial decision testable from
//! fixtures without network access.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use arbkit_core::{MarketId, OutcomeId, VenueId};
use arbkit_match::{
    lookup_team_unique, parse_kalshi_ticker, parse_poly_token_id, CanonicalRegistry, CanonicalTeam,
    MarketKind, OutcomeSide, Sport, VenueInstrument, VenueInstrumentMap, VenueRegistry,
};
use thiserror::Error;

/// Public Kalshi markets endpoint (trade API v2).
pub const KALSHI_MARKETS_URL: &str = "https://api.elections.kalshi.com/trade-api/v2/markets";

/// Public Polymarket Gamma events endpoint.
pub const POLYMARKET_GAMMA_EVENTS_URL: &str = "https://gamma-api.polymarket.com/events";

/// Errors raised while discovering venue markets over REST.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    /// Transport-level failure (DNS, connect, timeout, body read).
    #[error("discovery http transport failed: {0}")]
    Http(String),
    /// The venue answered with a non-success status code.
    #[error("discovery received http status {0}")]
    Status(u16),
    /// A response page could not be parsed at all.
    #[error("discovery page was not valid json for {venue}: {message}")]
    Page {
        /// Interned venue identifier whose page failed to parse.
        venue: VenueId,
        /// Serializer message.
        message: String,
    },
}

/// Counters describing one discovery pass. Skipped records are always
/// counted; the pessimistic reading is that anything skipped does not exist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiscoveryStats {
    /// Response pages fetched successfully.
    pub pages_fetched: u32,
    /// Raw market/event records seen across all pages.
    pub raw_records: u32,
    /// Instruments that were normalized and registered into the generation.
    pub parsed_instruments: u32,
    /// Records discarded because they were malformed or unresolvable.
    pub skipped_malformed: u32,
    /// Records discarded because they were closed or not tradable.
    pub skipped_not_tradable: u32,
    /// Polymarket propositions whose title and outcome labels could not be
    /// reconciled into an unambiguous club pair.
    pub skipped_ambiguous_orientation: u32,
    /// Polymarket propositions with no unique open Kalshi counterpart event.
    pub skipped_unmatched_event: u32,
}

impl DiscoveryStats {
    /// Add the counters of `other` into `self`.
    pub fn merge(&mut self, other: Self) {
        self.pages_fetched += other.pages_fetched;
        self.raw_records += other.raw_records;
        self.parsed_instruments += other.parsed_instruments;
        self.skipped_malformed += other.skipped_malformed;
        self.skipped_not_tradable += other.skipped_not_tradable;
        self.skipped_ambiguous_orientation += other.skipped_ambiguous_orientation;
        self.skipped_unmatched_event += other.skipped_unmatched_event;
    }
}

/// Configuration for paginating the Kalshi markets endpoint.
#[derive(Debug, Clone)]
pub struct KalshiDiscoveryConfig {
    /// Markets endpoint URL.
    pub markets_url: String,
    /// Page size requested per fetch (capped at 1000 by the venue).
    pub page_limit: u32,
    /// Maximum number of pages fetched per refresh.
    pub max_pages: u32,
}

impl Default for KalshiDiscoveryConfig {
    fn default() -> Self {
        Self {
            markets_url: KALSHI_MARKETS_URL.to_string(),
            page_limit: 100,
            max_pages: 10,
        }
    }
}

/// Configuration for paginating the Polymarket Gamma events endpoint.
#[derive(Debug, Clone)]
pub struct PolymarketDiscoveryConfig {
    /// Events endpoint URL.
    pub events_url: String,
    /// Page size requested per fetch.
    pub page_limit: u32,
    /// Maximum number of pages fetched per refresh.
    pub max_pages: u32,
}

impl Default for PolymarketDiscoveryConfig {
    fn default() -> Self {
        Self {
            events_url: POLYMARKET_GAMMA_EVENTS_URL.to_string(),
            page_limit: 100,
            max_pages: 10,
        }
    }
}

/// One open Kalshi sports market discovered over REST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredKalshiMarket {
    /// Full market ticker (e.g. `KXNBAGAME-26AUG181930BOSLAL-BOS`).
    pub ticker: String,
}

/// A Polymarket binary proposition whose clubs resolved unambiguously.
///
/// `first_token` is bound to `first_team` and `second_token` to `second_team`
/// by their outcome labels — not by array order alone. Home/away sides do not
/// exist at this stage; they are assigned from the Kalshi fixture when the
/// proposition pairs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPolymarketProposition {
    /// CLOB token id (decimal string) bound to `first_team`.
    pub first_token: String,
    /// CLOB token id (decimal string) bound to `second_team`.
    pub second_token: String,
    /// Club bound to `first_token` by its outcome label.
    pub first_team: &'static CanonicalTeam,
    /// Club bound to `second_token` by its outcome label.
    pub second_team: &'static CanonicalTeam,
    /// Game date in US Eastern calendar days (`YYYY-MM-DD`), taken from the
    /// venue's own game-start stamp. Pairing demands an exact match so a
    /// stale listing can never bind to a later rematch between the clubs.
    pub game_date: String,
}

/// One immutable catalog snapshot.
///
/// `registry` holds the canonical ids minted for this generation; `map`
/// holds only the validated cross-venue pairs eligible for execution.
#[derive(Debug, Clone)]
pub struct CatalogGeneration {
    /// Monotonic generation number; 0 is the empty pre-discovery state.
    pub generation: u64,
    /// Canonical registry backing the ids in `map`.
    pub registry: Arc<CanonicalRegistry>,
    /// Validated active cross-venue pairs.
    pub map: Arc<VenueInstrumentMap>,
    /// Build report for observability and Linear evidence.
    pub report: CatalogBuildReport,
}

impl CatalogGeneration {
    /// An empty generation with nothing active.
    pub fn empty(generation: u64) -> Self {
        Self {
            generation,
            registry: Arc::new(CanonicalRegistry::new()),
            map: Arc::new(VenueInstrumentMap::new()),
            report: CatalogBuildReport {
                generation,
                stats: DiscoveryStats::default(),
                canonical_events: 0,
                active_pairs: 0,
                paired_markets: 0,
            },
        }
    }
}

/// Summary of one catalog build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogBuildReport {
    /// Generation this report describes.
    pub generation: u64,
    /// Combined discovery counters.
    pub stats: DiscoveryStats,
    /// Distinct canonical events indexed from Kalshi tickers.
    pub canonical_events: usize,
    /// Active pairs installed in the map.
    pub active_pairs: usize,
    /// Distinct canonical markets that carry an active pair.
    pub paired_markets: usize,
}

/// Holder for the current catalog generation.
///
/// Readers clone the current `Arc` under a short lock and never touch the
/// lock again; refreshes replace the pointer wholesale. This type is not on
/// the engine hot loop.
#[derive(Debug)]
pub struct CatalogService {
    inner: RwLock<Arc<CatalogGeneration>>,
}

impl Default for CatalogService {
    fn default() -> Self {
        Self::new()
    }
}

impl CatalogService {
    /// Create a service starting from the empty generation 0.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Arc::new(CatalogGeneration::empty(0))),
        }
    }

    /// Snapshot of the currently installed generation.
    pub fn current(&self) -> Arc<CatalogGeneration> {
        let guard = self.inner.read().expect("catalog service lock poisoned");
        Arc::clone(&guard)
    }

    /// Install `next` atomically and return its stored snapshot.
    pub fn install(&self, next: CatalogGeneration) -> Arc<CatalogGeneration> {
        let arc = Arc::new(next);
        *self.inner.write().expect("catalog service lock poisoned") = Arc::clone(&arc);
        arc
    }
}

/// Thin reqwest-backed page source used by the discovery functions.
pub struct RestDiscoverySource {
    http: reqwest::Client,
}

impl RestDiscoverySource {
    /// Build a client with conservative timeouts.
    pub fn new() -> Result<Self, DiscoveryError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("arbkit-discovery/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| DiscoveryError::Http(e.to_string()))?;
        Ok(Self { http })
    }

    /// Fetch one URL and return the raw body text.
    pub async fn fetch_text(&self, url: &str) -> Result<String, DiscoveryError> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| DiscoveryError::Http(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(DiscoveryError::Status(status.as_u16()));
        }
        response
            .text()
            .await
            .map_err(|e| DiscoveryError::Http(e.to_string()))
    }
}

impl Default for RestDiscoverySource {
    fn default() -> Self {
        Self::new().expect("rest discovery client")
    }
}

/// Fetch all configured Kalshi pages and return the open sports tickers found.
pub async fn discover_kalshi_markets(
    source: &RestDiscoverySource,
    config: &KalshiDiscoveryConfig,
) -> Result<(Vec<DiscoveredKalshiMarket>, DiscoveryStats), DiscoveryError> {
    let mut stats = DiscoveryStats::default();
    let mut discovered: Vec<DiscoveredKalshiMarket> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cursor = String::new();

    for _ in 0..config.max_pages.max(1) {
        // Operator configs may pin the URL to one series or tag, so respect
        // an existing query string instead of colliding a second `?`.
        let sep = if config.markets_url.contains('?') {
            '&'
        } else {
            '?'
        };
        let mut url = format!(
            "{}{}status=open&limit={}",
            config.markets_url,
            sep,
            config.page_limit.clamp(1, 1000)
        );
        if !cursor.is_empty() {
            url.push_str("&cursor=");
            url.push_str(&cursor);
        }

        let body = source.fetch_text(&url).await?;
        stats.pages_fetched += 1;
        cursor = parse_kalshi_page(&body, &mut stats, &mut seen, &mut discovered);
        if cursor.is_empty() {
            break;
        }
    }

    Ok((discovered, stats))
}

/// Fetch all configured Polymarket pages and return resolvable propositions.
pub async fn discover_polymarket_propositions(
    source: &RestDiscoverySource,
    config: &PolymarketDiscoveryConfig,
) -> Result<(Vec<DiscoveredPolymarketProposition>, DiscoveryStats), DiscoveryError> {
    let mut stats = DiscoveryStats::default();
    let mut discovered: Vec<DiscoveredPolymarketProposition> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let limit = config.page_limit.clamp(1, 500);
    let mut offset = 0u64;

    for _ in 0..config.max_pages.max(1) {
        let sep = if config.events_url.contains('?') {
            '&'
        } else {
            '?'
        };
        let url = format!(
            "{}{}closed=false&limit={}&offset={}",
            config.events_url, sep, limit, offset
        );
        let body = source.fetch_text(&url).await?;
        stats.pages_fetched += 1;
        let events_on_page = parse_polymarket_page(&body, &mut stats, &mut seen, &mut discovered);
        offset += u64::from(limit);
        if events_on_page < limit {
            break;
        }
    }

    Ok((discovered, stats))
}

/// Discover both venues, build the next catalog generation, and install it.
pub async fn refresh_catalog(
    service: &CatalogService,
    source: &RestDiscoverySource,
    kalshi_config: &KalshiDiscoveryConfig,
    polymarket_config: &PolymarketDiscoveryConfig,
) -> Result<Arc<CatalogGeneration>, DiscoveryError> {
    let previous_generation = service.current().generation;

    let (kalshi, kalshi_stats) = discover_kalshi_markets(source, kalshi_config).await?;
    let (polymarket, poly_stats) =
        discover_polymarket_propositions(source, polymarket_config).await?;

    let mut stats = kalshi_stats;
    stats.merge(poly_stats);

    let next = build_catalog_generation(
        previous_generation.wrapping_add(1),
        &kalshi,
        &polymarket,
        stats,
    );
    Ok(service.install(next))
}

/// Build one complete catalog generation from already-discovered instruments.
///
/// Deterministic: inputs are processed in sorted order so canonical id
/// assignment is reproducible for identical discovery results.
pub fn build_catalog_generation(
    generation: u64,
    kalshi: &[DiscoveredKalshiMarket],
    polymarket: &[DiscoveredPolymarketProposition],
    mut stats: DiscoveryStats,
) -> CatalogGeneration {
    let mut registry = CanonicalRegistry::new();

    // Phase 1: register every parseable open Kalshi ticker, grouping the
    // moneyline markets they created by canonical event key.
    let groups = register_kalshi_side(&mut registry, kalshi, &mut stats);

    // Phase 2: index groups by an orientation-free club-pair key, so a
    // `vs.`-titled proposition meets its event regardless of which club the
    // title listed first.
    let mut index: HashMap<(&'static str, &'static str), Vec<&EventGroup>> = HashMap::new();
    for group in groups.values() {
        index
            .entry(code_pair_key(group.home_code, group.away_code))
            .or_default()
            .push(group);
    }

    // Phase 3: pair each resolvable Polymarket proposition with its unique
    // Kalshi moneyline market through the validated registration gate.
    let mut map = VenueInstrumentMap::new();
    let mut props: Vec<&DiscoveredPolymarketProposition> = polymarket.iter().collect();
    props.sort_by(|a, b| (&a.first_token, &a.second_token).cmp(&(&b.first_token, &b.second_token)));
    let mut paired_markets = 0usize;
    let mut paired_market_ids: HashSet<MarketId> = HashSet::new();

    for prop in props {
        let key = code_pair_key(prop.first_team.code, prop.second_team.code);
        let group = match index.get(&key) {
            // Same-club rematches across days are separate events; the
            // proposition's own game date picks exactly one, and anything
            // else (no candidate, or several on that date) never pairs.
            Some(candidates) => {
                let dated: Vec<_> = candidates
                    .iter()
                    .filter(|g| g.date == prop.game_date)
                    .collect();
                match dated.as_slice() {
                    [only] => **only,
                    _ => {
                        stats.skipped_unmatched_event += 1;
                        continue;
                    }
                }
            }
            None => {
                stats.skipped_unmatched_event += 1;
                continue;
            }
        };

        let moneyline = match group
            .moneylines
            .values()
            .find(|m| m.home_leg.is_some() && m.away_leg.is_some())
        {
            Some(m) => m,
            None => {
                stats.skipped_unmatched_event += 1;
                continue;
            }
        };

        let market_id = moneyline.market_id;

        // One canonical market gets exactly one Polymarket binding. A second
        // proposition naming the same fixture (e.g. a mirrored listing) must
        // not overwrite the first pair or pollute the registry; it is counted
        // and dropped.
        if !paired_market_ids.insert(market_id) {
            stats.skipped_malformed += 1;
            continue;
        }

        // Sides come from the Kalshi fixture's home/away reading, never from
        // Polymarket title order.
        let (home_token, away_token) = if group.home_code == prop.first_team.code {
            (&prop.first_token, &prop.second_token)
        } else {
            (&prop.second_token, &prop.first_token)
        };

        let (Some((home_ticker, home_outcome)), Some((_, away_outcome))) =
            (moneyline.home_leg.clone(), moneyline.away_leg.clone())
        else {
            stats.skipped_unmatched_event += 1;
            continue;
        };

        // Registry layout guarantees outcomes[0] is home and outcomes[1] is
        // away for moneyline markets created by CanonicalRegistry.
        if registry
            .register_polymarket_tokens(market_id, home_token, away_token)
            .is_err()
        {
            stats.skipped_malformed += 1;
            continue;
        }
        let poly_away_token = match parse_poly_token_id(away_token) {
            Ok(token) => token,
            Err(_) => {
                stats.skipped_malformed += 1;
                continue;
            }
        };

        let registered = map.register_binary_pair(
            market_id,
            MarketKind::Moneyline,
            (
                VenueInstrument {
                    venue: VenueRegistry::KALSHI,
                    market_id,
                    outcome_id: home_outcome,
                    kalshi_ticker: Some(home_ticker),
                    poly_token_id: None,
                },
                OutcomeSide::Home,
            ),
            (
                VenueInstrument {
                    venue: VenueRegistry::POLYMARKET,
                    market_id,
                    outcome_id: away_outcome,
                    kalshi_ticker: None,
                    poly_token_id: Some(poly_away_token),
                },
                OutcomeSide::Away,
            ),
        );
        if registered.is_err() {
            stats.skipped_malformed += 1;
            continue;
        }

        stats.parsed_instruments += 1;
        paired_markets += 1;
    }

    let report = CatalogBuildReport {
        generation,
        stats,
        canonical_events: groups.len(),
        active_pairs: map.len(),
        paired_markets,
    };

    CatalogGeneration {
        generation,
        registry: Arc::new(registry),
        map: Arc::new(map),
        report,
    }
}

struct MoneylineInfo {
    market_id: MarketId,
    /// `(ticker, outcome id)` for the canonical home side.
    home_leg: Option<(String, OutcomeId)>,
    /// `(ticker, outcome id)` for the canonical away side.
    away_leg: Option<(String, OutcomeId)>,
}

struct EventGroup {
    date: String,
    home_code: &'static str,
    away_code: &'static str,
    moneylines: BTreeMap<MarketId, MoneylineInfo>,
}

fn sport_rank(sport: Sport) -> u8 {
    match sport {
        Sport::Nba => 0,
        Sport::Nfl => 1,
        Sport::Mlb => 2,
        Sport::Nhl => 3,
        Sport::Soccer => 4,
        Sport::Other => 5,
    }
}

/// Orientation-free key for a club pair: sorted codes, so `BOS @ LAL` and
/// `LAL vs. BOS` land on the same event.
fn code_pair_key(a: &'static str, b: &'static str) -> (&'static str, &'static str) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

type GroupKey = (String, &'static str, &'static str, u8);

fn register_kalshi_side(
    registry: &mut CanonicalRegistry,
    kalshi: &[DiscoveredKalshiMarket],
    stats: &mut DiscoveryStats,
) -> BTreeMap<GroupKey, EventGroup> {
    let mut groups: BTreeMap<GroupKey, EventGroup> = BTreeMap::new();

    let mut tickers: Vec<&str> = kalshi.iter().map(|d| d.ticker.as_str()).collect();
    tickers.sort_unstable();
    tickers.dedup();

    for ticker in tickers {
        let parsed = match parse_kalshi_ticker(ticker) {
            Ok(parsed) => parsed,
            Err(_) => {
                stats.skipped_malformed += 1;
                continue;
            }
        };
        let kind = match parsed.market_kind {
            Some(kind) => kind,
            None => {
                stats.skipped_malformed += 1;
                continue;
            }
        };
        let has_explicit_outcome = match kind {
            MarketKind::Moneyline | MarketKind::Spread(_) => parsed.target_team.is_some(),
            MarketKind::Total(_) => parsed.is_over.is_some(),
        };
        if !has_explicit_outcome {
            stats.skipped_malformed += 1;
            continue;
        }

        let outcome_id = match registry.register_kalshi_ticker(ticker) {
            Ok(id) => id,
            Err(_) => {
                stats.skipped_malformed += 1;
                continue;
            }
        };
        stats.parsed_instruments += 1;

        if !matches!(kind, MarketKind::Moneyline) {
            // Registered for feed completeness; spreads/totals have no
            // Polymarket counterpart parser yet, so they never pair.
            continue;
        }

        let Some(outcome) = registry.get_outcome(outcome_id) else {
            stats.skipped_malformed += 1;
            continue;
        };
        let market_id = outcome.market_id;
        if registry.get_market(market_id).is_none() {
            stats.skipped_malformed += 1;
            continue;
        }

        let target = parsed.target_team.unwrap_or(parsed.matchup.home);
        let is_home_leg = target == parsed.matchup.home;
        let key: GroupKey = (
            parsed.date_code.clone(),
            parsed.matchup.home.code,
            parsed.matchup.away.code,
            sport_rank(parsed.sport),
        );

        let group = groups.entry(key).or_insert_with(|| EventGroup {
            date: parsed.date_code.clone(),
            home_code: parsed.matchup.home.code,
            away_code: parsed.matchup.away.code,
            moneylines: BTreeMap::new(),
        });

        let info = group.moneylines.entry(market_id).or_insert(MoneylineInfo {
            market_id,
            home_leg: None,
            away_leg: None,
        });
        let leg = (ticker.to_string(), outcome_id);
        if is_home_leg && info.home_leg.is_none() {
            info.home_leg = Some(leg);
        } else if !is_home_leg && info.away_leg.is_none() {
            info.away_leg = Some(leg);
        }
    }

    groups
}

/// Parse one Kalshi `/markets` page. Returns the continuation cursor.
fn parse_kalshi_page(
    body: &str,
    stats: &mut DiscoveryStats,
    seen: &mut HashSet<String>,
    out: &mut Vec<DiscoveredKalshiMarket>,
) -> String {
    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => {
            stats.skipped_malformed += 1;
            return String::new();
        }
    };

    if let Some(markets) = value.get("markets").and_then(|m| m.as_array()) {
        for market in markets {
            stats.raw_records += 1;
            // Kalshi's REST filter takes `status=open`, but the returned
            // records stamp themselves `active`; accept both spellings.
            let status_open = market
                .get("status")
                .and_then(|s| s.as_str())
                .is_none_or(|s| s.eq_ignore_ascii_case("open") || s.eq_ignore_ascii_case("active"));
            if !status_open {
                stats.skipped_not_tradable += 1;
                continue;
            }
            let Some(ticker) = market.get("ticker").and_then(|t| t.as_str()) else {
                stats.skipped_malformed += 1;
                continue;
            };
            if seen.insert(ticker.to_string()) {
                out.push(DiscoveredKalshiMarket {
                    ticker: ticker.to_string(),
                });
            }
        }
    } else {
        stats.skipped_malformed += 1;
    }

    value
        .get("cursor")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string()
}

/// Parse one Gamma events page. Returns the number of events on the page.
fn parse_polymarket_page(
    body: &str,
    stats: &mut DiscoveryStats,
    seen: &mut HashSet<(String, String)>,
    out: &mut Vec<DiscoveredPolymarketProposition>,
) -> u32 {
    let root: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => {
            stats.skipped_malformed += 1;
            return 0;
        }
    };
    let events = match root.as_array() {
        Some(events) => events.clone(),
        None => match root.get("data").and_then(|d| d.as_array()) {
            Some(events) => events.clone(),
            None => {
                stats.skipped_malformed += 1;
                return 0;
            }
        },
    };

    let mut count = 0u32;
    for event in &events {
        count += 1;
        if event
            .get("closed")
            .and_then(|c| c.as_bool())
            .unwrap_or(false)
        {
            stats.skipped_not_tradable += 1;
            continue;
        }
        let Some(markets) = event.get("markets").and_then(|m| m.as_array()) else {
            continue;
        };
        for market in markets {
            stats.raw_records += 1;
            if market
                .get("closed")
                .and_then(|c| c.as_bool())
                .unwrap_or(false)
                || !market
                    .get("active")
                    .and_then(|a| a.as_bool())
                    .unwrap_or(true)
                || !market
                    .get("enableOrderBook")
                    .and_then(|e| e.as_bool())
                    .unwrap_or(true)
            {
                stats.skipped_not_tradable += 1;
                continue;
            }
            let Some(question) = market
                .get("question")
                .and_then(|q| q.as_str())
                .or_else(|| market.get("title").and_then(|t| t.as_str()))
            else {
                stats.skipped_malformed += 1;
                continue;
            };
            let outcomes = string_list(market.get("outcomes"));
            let tokens = string_list(market.get("clobTokenIds"));
            let game_start = market.get("gameStartTime").and_then(|g| g.as_str());
            if let Some(prop) = resolve_proposition(question, &outcomes, &tokens, game_start, stats)
            {
                if seen.insert((prop.first_token.clone(), prop.second_token.clone())) {
                    out.push(prop);
                }
            }
        }
    }

    count
}

/// Resolve one Polymarket market into a proposition, or explain the skip.
fn resolve_proposition(
    question: &str,
    outcomes: &[String],
    tokens: &[String],
    game_start: Option<&str>,
    stats: &mut DiscoveryStats,
) -> Option<DiscoveredPolymarketProposition> {
    if outcomes.len() != 2 || tokens.len() != 2 || tokens.iter().any(String::is_empty) {
        stats.skipped_malformed += 1;
        return None;
    }

    // The title must name exactly two clubs; `vs.` titles are accepted
    // because sides no longer depend on title order.
    let Some((left_raw, right_raw)) = split_title_teams(question) else {
        stats.skipped_ambiguous_orientation += 1;
        return None;
    };
    let left = match lookup_team_unique(left_raw) {
        Ok(team) => team,
        Err(_) => {
            stats.skipped_malformed += 1;
            return None;
        }
    };
    let right = match lookup_team_unique(right_raw) {
        Ok(team) => team,
        Err(_) => {
            stats.skipped_malformed += 1;
            return None;
        }
    };
    if left == right || left.sport != right.sport {
        stats.skipped_malformed += 1;
        return None;
    }

    // Bind each token to its club through its own outcome label: Gamma lists
    // `outcomes[i]` alongside `clobTokenIds[i]`, and full team names resolve
    // uniquely. A label that does not belong to the titled pair means this
    // market is not the game it appears to be.
    let first = match lookup_team_unique(&outcomes[0]) {
        Ok(team) => team,
        Err(_) => {
            stats.skipped_malformed += 1;
            return None;
        }
    };
    let second = match lookup_team_unique(&outcomes[1]) {
        Ok(team) => team,
        Err(_) => {
            stats.skipped_malformed += 1;
            return None;
        }
    };
    let titled = [left, right];
    let labeled = [first, second];
    if labeled[0] == labeled[1] || !titled.contains(&labeled[0]) || !titled.contains(&labeled[1]) {
        stats.skipped_ambiguous_orientation += 1;
        return None;
    }

    // The game's own start stamp dates the fixture; without it the
    // proposition could silently bind to any open rematch of these clubs.
    let game_date = match game_start.and_then(eastern_game_date) {
        Some(date) => date,
        None => {
            stats.skipped_malformed += 1;
            return None;
        }
    };

    Some(DiscoveredPolymarketProposition {
        first_token: tokens[0].clone(),
        second_token: tokens[1].clone(),
        first_team: first,
        second_team: second,
        game_date,
    })
}

/// Split a game title into its two club labels.
///
/// Accepts `"Away @ Home"`, `"Away at Home"`, and orientation-free `"A vs. B"`
/// forms, trimming trailing clauses like `": Who will win?"`. Anything without
/// one of these separators carries no team-pair structure and returns `None`.
fn split_title_teams(title: &str) -> Option<(&str, &str)> {
    const SEPARATORS: [&str; 6] = [" @ ", " at ", " vs. ", " vs ", " v. ", " v "];

    let lower = title.to_lowercase();
    let mut best: Option<(usize, usize)> = None;
    for separator in SEPARATORS {
        if let Some(idx) = lower.find(separator) {
            best = match best {
                Some((best_idx, _)) if best_idx <= idx => best,
                _ => Some((idx, separator.len())),
            };
        }
    }
    let (idx, sep_len) = best?;

    let left_raw = title[..idx].trim();
    let right_raw = title[idx + sep_len..].trim();
    let left_raw = first_clause(left_raw)?;
    let right_raw = first_clause(right_raw)?;
    if left_raw.is_empty() || right_raw.is_empty() {
        return None;
    }
    Some((left_raw, right_raw))
}

fn first_clause(part: &str) -> Option<&str> {
    let without_colon = part.split(':').next()?;
    let without_question = without_colon.split('?').next()?.trim();
    let without_dash = without_question.split(" - ").next()?.trim();
    if without_dash.is_empty() {
        None
    } else {
        Some(without_dash)
    }
}

/// Calendar date (`YYYY-MM-DD`) in US Eastern time of a Gamma game-start
/// stamp.
///
/// Gamma reports `gameStartTime` in UTC ("2026-08-26 22:40:00+00") while the
/// Kalshi ticker embeds the game's US Eastern day, so pairing compares both
/// only after shifting. DST follows the US window (EDT from the second
/// Sunday of March through the first Sunday of November); instants inside
/// the 02:00 local switch resolve as EDT, which no scheduled game start
/// touches. Implemented by hand — the hot-path crates carry no chrono.
fn eastern_game_date(raw: &str) -> Option<String> {
    let b = raw.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let year: i64 = raw.get(..4)?.parse().ok()?;
    let month: u32 = raw.get(5..7)?.parse().ok()?;
    let day: u32 = raw.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Time of day: HH:MM[:SS], optionally fractional; absent means midnight.
    let tail = &raw[10..];
    let time = tail.trim_start_matches(['T', ' ']);
    let mut fields = time.split(|c: char| !c.is_ascii_digit());
    let hour: i64 = fields
        .next()
        .filter(|f| !f.is_empty())
        .unwrap_or("0")
        .parse()
        .ok()?;
    let minute: i64 = fields.next().and_then(|f| f.parse().ok()).unwrap_or(0);
    let second: i64 = fields.next().and_then(|f| f.parse().ok()).unwrap_or(0);
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }

    // Explicit offset ("+00", "-04:00", "Z"); anything unparsable is UTC,
    // which is what Gamma ships. The scan starts after the date so the
    // dashes in `YYYY-MM-DD` are never mistaken for a sign.
    let offset_secs = match (tail.find('+'), tail.rfind('-')) {
        (Some(p), q) if p < q.unwrap_or(p) => parse_utc_offset(&tail[p + 1..])?,
        (_, Some(q)) => -parse_utc_offset(&tail[q + 1..])?,
        _ => 0,
    };

    let utc_epoch = days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second
        - offset_secs;

    // Decide EDT against the local reading itself so boundary days land on
    // one side deterministically.
    let edt = is_us_dst(utc_epoch - 4 * 3600);
    let et_epoch = utc_epoch - if edt { 4 * 3600 } else { 5 * 3600 };
    let (y, m, d) = civil_from_days(et_epoch.div_euclid(86400));
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

fn parse_utc_offset(raw: &str) -> Option<i64> {
    let digits: String = raw
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ':')
        .collect();
    let mut fields = digits.split(':');
    let hh: i64 = fields.next()?.parse().ok()?;
    let mm: i64 = fields.next().and_then(|f| f.parse().ok()).unwrap_or(0);
    if !(0..=14).contains(&hh) || !(0..=59).contains(&mm) {
        return None;
    }
    Some(hh * 3600 + mm * 60)
}

fn is_us_dst(local_epoch: i64) -> bool {
    let (year, month, day) = civil_from_days(local_epoch.div_euclid(86400));
    match month {
        4..=10 => true,
        3 => day >= nth_weekday_of_month(year, 3, 0, 2),
        11 => day < nth_weekday_of_month(year, 11, 0, 1),
        _ => false,
    }
}

/// Date (day-of-month) of the `n`-th Sunday (`weekday == 0`) of a month.
fn nth_weekday_of_month(year: i64, month: u32, weekday: u32, n: u32) -> u32 {
    let first_weekday = weekday_of(days_from_civil(year, month, 1));
    let first = 1 + (weekday + 7 - first_weekday) % 7;
    first + 7 * (n - 1)
}

/// Days since 1970-01-01 (Howard Hinnant's civil calendar algorithms).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = ((mp + 2) % 12 + 1) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Weekday index of an epoch-day count, Sunday == 0 (1970-01-01 was Thursday).
fn weekday_of(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

fn string_list(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        // Gamma often encodes arrays as JSON-in-a-string.
        Some(serde_json::Value::String(raw)) => {
            serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KALSHI_PAGE: &str = r#"{
        "markets": [
            {"ticker": "KXNBAGAME-26AUG181930BOSLAL-BOS", "status": "open"},
            {"ticker": "KXNBAGAME-26AUG181930BOSLAL-LAL", "status": "open"},
            {"ticker": "KXNBAGAME-26AUG181930BOSLAL-BOS", "status": "closed"},
            {"ticker": "KXNBASPREAD-26AUG181930BOSLAL-LAL35", "status": "open"},
            {"ticker": "NOT-A-REAL-SERIES-XX", "status": "open"}
        ],
        "cursor": ""
    }"#;

    const POLY_PAGE_GOOD: &str = r#"[{
        "closed": false,
        "markets": [
            {
                "question": "Boston Celtics @ Los Angeles Lakers",
                "gameStartTime": "2026-08-18 23:30:00+00",
                "outcomes": "[\"Boston Celtics\", \"Los Angeles Lakers\"]",
                "clobTokenIds": "[\"52222696630142282340910360142526626600364417636157306499739927226918483751010\", \"62684925644295939715237367951956374449407613715370329023488979813\"]",
                "closed": false,
                "active": true,
                "enableOrderBook": true
            },
            {
                "question": "Boston Celtics vs. Los Angeles Lakers",
                "gameStartTime": "2026-08-18 23:30:00+00",
                "outcomes": "[\"Boston Celtics\", \"Los Angeles Lakers\"]",
                "clobTokenIds": "[\"1\", \"2\"]",
                "closed": false,
                "active": true
            },
            {
                "question": "Will the Lakers beat the Celtics?",
                "gameStartTime": "2026-08-18 23:30:00+00",
                "outcomes": "[\"Yes\", \"No\"]",
                "clobTokenIds": "[\"3\", \"4\"]",
                "closed": false,
                "active": true
            },
            {
                "question": "Boston Celtics @ Los Angeles Lakers",
                "gameStartTime": "2026-08-18 23:30:00+00",
                "outcomes": "[\"Los Angeles Lakers\", \"Boston Celtics\"]",
                "clobTokenIds": "[\"5\", \"6\"]",
                "closed": false,
                "active": true
            }
        ]
    }]"#;

    #[test]
    fn kalshi_page_keeps_open_records_and_cursor() {
        let mut stats = DiscoveryStats::default();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let cursor = parse_kalshi_page(KALSHI_PAGE, &mut stats, &mut seen, &mut out);

        assert_eq!(cursor, "");
        assert_eq!(stats.raw_records, 5);
        assert_eq!(stats.skipped_not_tradable, 1);
        assert_eq!(
            out,
            vec![
                DiscoveredKalshiMarket {
                    ticker: "KXNBAGAME-26AUG181930BOSLAL-BOS".into()
                },
                DiscoveredKalshiMarket {
                    ticker: "KXNBAGAME-26AUG181930BOSLAL-LAL".into()
                },
                DiscoveredKalshiMarket {
                    ticker: "KXNBASPREAD-26AUG181930BOSLAL-LAL35".into()
                },
                DiscoveredKalshiMarket {
                    ticker: "NOT-A-REAL-SERIES-XX".into()
                },
            ]
        );
    }

    #[test]
    fn kalshi_page_reports_invalid_json_without_panicking() {
        let mut stats = DiscoveryStats::default();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let cursor = parse_kalshi_page("not json", &mut stats, &mut seen, &mut out);
        assert_eq!(cursor, "");
        assert_eq!(stats.skipped_malformed, 1);
        assert!(out.is_empty());
    }

    #[test]
    fn polymarket_page_resolves_at_vs_and_rejects_inconsistent_labels() {
        let mut stats = DiscoveryStats::default();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let events = parse_polymarket_page(POLY_PAGE_GOOD, &mut stats, &mut seen, &mut out);

        assert_eq!(events, 1);
        // closed=false event; four inner markets counted raw.
        assert_eq!(stats.raw_records, 4);
        // "Will X beat Y" names no club pair in its title; the reversed
        // outcome listing binds by label and resolves on its own tokens.
        assert_eq!(stats.skipped_ambiguous_orientation, 1);
        assert_eq!(stats.skipped_malformed, 0);
        assert_eq!(out.len(), 3);

        let prop = &out[0];
        assert_eq!(prop.first_team.code, "BOS");
        assert_eq!(prop.second_team.code, "LAL");
        assert!(prop.first_token.starts_with('5'));

        // The `vs.`-titled twin binds the same clubs to its own tokens.
        assert_eq!(out[1].first_team.code, "BOS");
        assert_eq!(out[1].second_token, "2");

        // The reversed listing binds each token to the club its label names,
        // regardless of array order.
        assert_eq!(out[2].first_team.code, "LAL");
        assert_eq!(out[2].second_token, "6");
    }

    #[test]
    fn city_alias_collisions_are_rejected() {
        let mut stats = DiscoveryStats::default();
        let prop = resolve_proposition(
            "Boston @ Los Angeles",
            &["Boston".into(), "Los Angeles".into()],
            &["11".into(), "22".into()],
            Some("2026-08-18 23:30:00+00"),
            &mut stats,
        );
        assert!(prop.is_none());
        // City aliases match multiple teams across sports: malformed, never guessed.
        assert_eq!(stats.skipped_malformed, 1);
    }

    #[test]
    fn split_handles_suffix_clauses_and_separator_forms() {
        assert_eq!(
            split_title_teams("Celtics at Lakers: Who wins?"),
            Some(("Celtics", "Lakers"))
        );
        assert_eq!(
            split_title_teams("Tampa Bay Rays vs. New York Yankees"),
            Some(("Tampa Bay Rays", "New York Yankees"))
        );
        assert_eq!(
            split_title_teams("Celtics vs Lakers"),
            Some(("Celtics", "Lakers"))
        );
        assert_eq!(split_title_teams("Lakers - Celtics"), None);
    }

    #[test]
    fn build_pairs_validated_moneyline_and_counts_skips() {
        let kalshi: Vec<DiscoveredKalshiMarket> = [
            "KXNBAGAME-26AUG181930BOSLAL-BOS",
            "KXNBAGAME-26AUG181930BOSLAL-LAL",
            "KXNBASPREAD-26AUG181930BOSLAL-LAL35",
            "KXNBATOTAL-26AUG181930BOSLAL-2205O",
        ]
        .iter()
        .map(|&ticker| DiscoveredKalshiMarket {
            ticker: ticker.into(),
        })
        .chain(std::iter::once(DiscoveredKalshiMarket {
            ticker: "KXNFLGAME-26SEP191300BALDAL-DAL".into(),
        }))
        .collect();

        let poly = vec![DiscoveredPolymarketProposition {
            first_token:
                "52222696630142282340910360142526626600364417636157306499739927226918483751010"
                    .into(),
            second_token: "62684925644295939715237367951956374449407613715370329023488979813"
                .into(),
            first_team: arbkit_match::lookup_team("Boston Celtics", Some(Sport::Nba)).unwrap(),
            second_team: arbkit_match::lookup_team("Los Angeles Lakers", Some(Sport::Nba)).unwrap(),
            game_date: "2026-08-18".into(),
        }];

        let gen = build_catalog_generation(1, &kalshi, &poly, DiscoveryStats::default());

        assert_eq!(gen.report.canonical_events, 2);
        assert_eq!(gen.report.active_pairs, 1);
        assert_eq!(gen.report.paired_markets, 1);
        assert_eq!(gen.map.len(), 1);

        // The pair legs sit on opposite sides of the same canonical market.
        let pair = gen.map.iter().next().unwrap();
        assert_ne!(pair.first.venue, pair.second.venue);
        let kalshi_leg_is_first = pair.first.venue == VenueRegistry::KALSHI;
        let kalshi_leg = if kalshi_leg_is_first {
            &pair.first
        } else {
            &pair.second
        };
        let poly_leg = if kalshi_leg_is_first {
            &pair.second
        } else {
            &pair.first
        };
        assert_eq!(kalshi_leg.market_id, poly_leg.market_id);
        assert!(kalshi_leg
            .kalshi_ticker
            .as_deref()
            .unwrap()
            .ends_with("-LAL"));
        assert_ne!(poly_leg.poly_token_id, None);

        // Registry exposes both tokens mapped onto the canonical outcomes.
        let home_outcome = gen.registry.resolve_venue_outcome(
            VenueRegistry::POLYMARKET,
            "62684925644295939715237367951956374449407613715370329023488979813",
        );
        let away_outcome = gen.registry.resolve_venue_outcome(
            VenueRegistry::POLYMARKET,
            "52222696630142282340910360142526626600364417636157306499739927226918483751010",
        );
        assert_ne!(home_outcome, away_outcome);
    }

    #[test]
    fn same_clubs_on_two_dates_pair_by_exact_date() {
        // A series: the same clubs play on consecutive days. A proposition
        // dated on one of them must bind to that day only — never to the
        // whole club pair — and a date with no open event stays unpaired.
        let kalshi: Vec<DiscoveredKalshiMarket> = [
            "KXNBAGAME-26AUG181930BOSLAL-BOS",
            "KXNBAGAME-26AUG181930BOSLAL-LAL",
            "KXNBAGAME-26AUG191930BOSLAL-BOS",
            "KXNBAGAME-26AUG191930BOSLAL-LAL",
        ]
        .iter()
        .map(|&ticker| DiscoveredKalshiMarket {
            ticker: ticker.into(),
        })
        .collect();

        let bos = arbkit_match::lookup_team("Boston Celtics", Some(Sport::Nba)).unwrap();
        let lal = arbkit_match::lookup_team("Los Angeles Lakers", Some(Sport::Nba)).unwrap();
        let poly = vec![
            DiscoveredPolymarketProposition {
                first_token: "10011001100110011001100110011001100110011001100110011001100110011001"
                    .into(),
                second_token:
                    "20022002200220022002200220022002200220022002200220022002200220022002".into(),
                first_team: bos,
                second_team: lal,
                game_date: "2026-08-18".into(),
            },
            DiscoveredPolymarketProposition {
                first_token: "30033003300330033003300330033003300330033003300330033003300330033"
                    .into(),
                second_token: "40044004400440044004400440044004400440044004400440044004400440044"
                    .into(),
                first_team: bos,
                second_team: lal,
                game_date: "2026-08-20".into(),
            },
        ];

        let gen = build_catalog_generation(1, &kalshi, &poly, DiscoveryStats::default());
        // The 18th proposition paired; the 20th found no open event.
        assert_eq!(gen.map.len(), 1);
        assert_eq!(gen.report.stats.skipped_unmatched_event, 1);

        // And it paired against the Aug 18 market specifically.
        let pair = gen.map.iter().next().unwrap();
        let ticker = if pair.first.venue == VenueRegistry::KALSHI {
            pair.first.kalshi_ticker.as_deref().unwrap()
        } else {
            pair.second.kalshi_ticker.as_deref().unwrap()
        };
        assert!(ticker.contains("26AUG181930"), "paired {ticker}");
    }

    #[test]
    fn listing_order_never_flips_sides_and_duplicate_bindings_skip() {
        // One live-shaped MLB fixture: TB at DET (Kalshi reads TB away, DET
        // home). Two propositions list the clubs in opposite orders with
        // different token pairs; both bind to the same canonical event.
        let kalshi: Vec<DiscoveredKalshiMarket> = [
            "KXMLBGAME-26AUG241840TBDET-TB",
            "KXMLBGAME-26AUG241840TBDET-DET",
        ]
        .iter()
        .map(|&ticker| DiscoveredKalshiMarket {
            ticker: ticker.into(),
        })
        .collect();

        let tb = arbkit_match::lookup_team("Tampa Bay Rays", Some(Sport::Mlb)).unwrap();
        let det = arbkit_match::lookup_team("Detroit Tigers", Some(Sport::Mlb)).unwrap();
        let poly = vec![
            DiscoveredPolymarketProposition {
                first_token: "1111111111111111111111111111111111111111".into(),
                second_token: "2222222222222222222222222222222222222222".into(),
                first_team: tb,
                second_team: det,
                game_date: "2026-08-24".into(),
            },
            DiscoveredPolymarketProposition {
                first_token: "3333333333333333333333333333333333333333".into(),
                second_token: "4444444444444444444444444444444444444444".into(),
                first_team: det,
                second_team: tb,
                game_date: "2026-08-24".into(),
            },
        ];

        let gen = build_catalog_generation(1, &kalshi, &poly, DiscoveryStats::default());

        // Exactly one pair survives; the reversed duplicate cannot rebind the
        // same canonical outcomes and is counted, never guessed.
        assert_eq!(gen.map.len(), 1);
        assert_eq!(gen.report.active_pairs, 1);
        assert_eq!(gen.report.stats.skipped_malformed, 1);

        // The winning binding follows the Kalshi reading: DET is home, so its
        // club token from the first proposition maps to the home outcome.
        let home_outcome = gen
            .registry
            .resolve_venue_outcome(
                VenueRegistry::POLYMARKET,
                "2222222222222222222222222222222222222222",
            )
            .expect("home token registered");
        let away_outcome = gen
            .registry
            .resolve_venue_outcome(
                VenueRegistry::POLYMARKET,
                "1111111111111111111111111111111111111111",
            )
            .expect("away token registered");
        assert_ne!(home_outcome, away_outcome);
        assert!(gen
            .registry
            .resolve_venue_outcome(
                VenueRegistry::POLYMARKET,
                "3333333333333333333333333333333333333333"
            )
            .is_none());

        // The stored Polymarket leg carries the away-side token.
        let pair = gen.map.iter().next().unwrap();
        let poly_leg = if pair.first.venue == VenueRegistry::KALSHI {
            &pair.second
        } else {
            &pair.first
        };
        assert_eq!(
            poly_leg.poly_token_id,
            Some(parse_poly_token_id("1111111111111111111111111111111111111111").unwrap())
        );
    }

    #[test]
    fn spread_and_total_tickers_register_but_never_pair() {
        let kalshi: Vec<DiscoveredKalshiMarket> = [
            "KXNBASPREAD-26AUG181930BOSLAL-LAL35",
            "KXNBATOTAL-26AUG181930BOSLAL-2205O",
        ]
        .iter()
        .map(|&ticker| DiscoveredKalshiMarket {
            ticker: ticker.into(),
        })
        .collect();
        let gen = build_catalog_generation(1, &kalshi, &[], DiscoveryStats::default());
        assert_eq!(gen.report.active_pairs, 0);
        assert_eq!(gen.map.len(), 0);
        // Both instruments were still registered canonically for feeds.
        assert_eq!(gen.report.stats.parsed_instruments, 2);
    }

    #[test]
    fn service_swap_preserves_installed_snapshots() {
        let service = CatalogService::new();
        assert_eq!(service.current().generation, 0);

        let first = service.install(CatalogGeneration::empty(1));
        let observed = service.current();
        assert!(Arc::ptr_eq(&first, &observed));

        let second = service.install(CatalogGeneration::empty(2));
        assert_eq!(second.generation, 2);
        // The previously returned snapshot is untouched by the swap.
        assert_eq!(observed.generation, 1);
        assert_eq!(service.current().generation, 2);
    }

    #[test]
    fn string_list_accepts_plain_and_stringified_arrays() {
        let plain = serde_json::json!(["a", "b"]);
        assert_eq!(
            string_list(Some(&plain)),
            vec!["a".to_string(), "b".to_string()]
        );
        let encoded = serde_json::json!("[\"a\",\"b\"]");
        assert_eq!(
            string_list(Some(&encoded)),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(string_list(None).is_empty());
        assert!(string_list(Some(&serde_json::json!(42))).is_empty());
    }

    /// End-to-end over real captured API bodies (Aug 2026 public endpoints,
    /// stored under `tests/fixtures/`): the live MLB slate must populate a
    /// catalog with validated cross-venue pairs, not skip its way to empty.
    #[test]
    fn live_slate_fixture_builds_a_real_catalog() {
        let dir = env!("CARGO_MANIFEST_DIR");
        let kalshi_body =
            std::fs::read_to_string(format!("{dir}/tests/fixtures/kalshi_mlb_page.json")).unwrap();
        let poly_body =
            std::fs::read_to_string(format!("{dir}/tests/fixtures/polymarket_mlb_games.json"))
                .unwrap();

        let mut stats = DiscoveryStats::default();
        let mut seen_kalshi = HashSet::new();
        let mut kalshi = Vec::new();
        parse_kalshi_page(&kalshi_body, &mut stats, &mut seen_kalshi, &mut kalshi);
        assert!(
            kalshi.len() >= 40,
            "open Kalshi moneylines: {}",
            kalshi.len()
        );

        let mut seen_poly = HashSet::new();
        let mut props = Vec::new();
        parse_polymarket_page(&poly_body, &mut stats, &mut seen_poly, &mut props);

        let gen = build_catalog_generation(1, &kalshi, &props, stats);
        let report = gen.report;
        eprintln!(
            "live slate: events={} pairs={} paired_markets={} \
             skipped(unmatched={} malformed={} ambiguous={} not_tradable={})",
            report.canonical_events,
            report.active_pairs,
            report.paired_markets,
            report.stats.skipped_unmatched_event,
            report.stats.skipped_malformed,
            report.stats.skipped_ambiguous_orientation,
            report.stats.skipped_not_tradable,
        );

        // The overlap between the two venues' open slates is dozens of
        // games; anything materially below that means the alignment logic
        // is silently discarding real pairs.
        assert!(report.canonical_events >= 20, "{report:?}");
        assert!(report.active_pairs >= 10, "{report:?}");
        // Every installed pair is a distinct canonical market.
        assert_eq!(report.active_pairs, gen.map.len());
    }
}
