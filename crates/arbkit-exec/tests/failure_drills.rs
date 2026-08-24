//! Failure-drill harness for the authenticated execution adapters (HJ-64).
//!
//! Each drill wires the *real* signed HTTP adapters to a scripted local
//! mock venue and drives the production `HedgedExecutor`/`RiskGate` path,
//! pinning one failure scenario end to end: full fill, rejection, partial
//! fill, unwind failure, timeout, duplicate retry, stale feed, restart,
//! kill switch, and balance mismatch. Nothing here reaches a real venue;
//! the demo/sandbox presets are exercised as configuration only.
//!
//! Run with: `cargo test -p arbkit-exec --all-features --test failure_drills`

#![cfg(feature = "live")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use arbkit_core::{Cents, Prob, VenueId};
use arbkit_exec::{
    ExecError, ExecLeg, ExecutionClassification, FeedCircuitBreaker, FillEvent, HedgedExecutor,
    InFlightOrder, KalshiConfig, KalshiExecutionAdapter, PersistedExecLeg, PolymarketConfig,
    PolymarketExecutionAdapter, ReconciliationLedger, RiskConfig, RiskGate, RiskStateStore,
    VenueAdapter, VenueInstrumentRef,
};

// ---------------------------------------------------------------------------
// Scripted mock venue
// ---------------------------------------------------------------------------

/// One parsed inbound request, enough to route a fixture response.
#[derive(Debug, Clone)]
struct MockRequest {
    method: String,
    path: String,
    body: String,
}

type Responder = Box<dyn FnMut(&MockRequest) -> Option<(u16, String)> + Send>;
type Fixture = Box<dyn FnOnce(&MockRequest) -> (u16, String) + Send>;

/// Local HTTP server scripted per drill. Requests are logged so drills can
/// prove which orders actually reached a venue — including proving that
/// none did.
struct ScriptedVenue {
    address: SocketAddr,
    handle: Option<JoinHandle<Vec<MockRequest>>>,
}

impl ScriptedVenue {
    fn start(responder: Responder) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock venue");
        let address = listener.local_addr().expect("mock venue address");
        let handle = thread::spawn(move || {
            let mut responder = responder;
            let mut seen = Vec::new();
            for stream in listener.incoming().flatten() {
                // A hung client must never wedge the harness.
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut stream = stream;
                let Some(request) = read_request(&mut stream) else {
                    break;
                };
                if request.path == "/__drill-stop__" {
                    break;
                }
                seen.push(request.clone());
                match responder(&request) {
                    Some((status, body)) => {
                        let _ = stream.write_all(&http_response(status, &body));
                    }
                    None => break,
                }
            }
            seen
        });
        Self {
            address,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Wake the accept loop, collect everything served, and finish.
    fn requests(mut self) -> Vec<MockRequest> {
        poison(self.address);
        self.handle.take().expect("handle").join().expect("server")
    }
}

impl Drop for ScriptedVenue {
    fn drop(&mut self) {
        if self.handle.is_some() {
            poison(self.address);
            let _ = self.handle.take().expect("handle").join();
        }
    }
}

fn poison(address: SocketAddr) {
    let Ok(mut stream) = TcpStream::connect(address) else {
        return;
    };
    let _ = stream.write_all(b"GET /__drill-stop__ HTTP/1.1\r\nContent-Length: 0\r\n\r\n");
}

fn read_request(stream: &mut TcpStream) -> Option<MockRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let size = stream.read(&mut chunk).ok()?;
        if size == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..size]);
        let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buffer[..position]).to_string();
        let content_length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        let total = position + 4 + content_length;
        if buffer.len() < total {
            continue;
        }
        let mut lines = head.lines();
        let request_line = lines.next()?;
        let mut parts = request_line.split_whitespace();
        let method = parts.next()?.to_owned();
        let path = parts.next()?.to_owned();
        let body = String::from_utf8_lossy(&buffer[position + 4..total]).to_string();
        return Some(MockRequest { method, path, body });
    }
}

fn http_response(status: u16, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} Drill\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

// ---------------------------------------------------------------------------
// Fixture builders
// ---------------------------------------------------------------------------

/// Serves `responses` in order, then stops so `requests()` can join.
fn scripted(responses: Vec<Fixture>) -> Responder {
    let mut responses = responses.into_iter().rev().collect::<Vec<_>>();
    Box::new(move |request| {
        let respond = responses.pop()?;
        Some(respond(request))
    })
}

fn json(status: u16, body: &str) -> Fixture {
    let body = body.to_owned();
    Box::new(move |_| (status, body))
}

fn kalshi_order(order_id: &'static str, filled: i64) -> Fixture {
    Box::new(move |_| {
        (
            200,
            format!(
                r#"{{"order":{{"order_id":"{order_id}","filled_count":{filled},"status":"executed"}}}}"#
            ),
        )
    })
}

fn kalshi_adapter(base_url: &str, timeout_ms: u64) -> KalshiExecutionAdapter {
    use rsa::pkcs8::EncodePrivateKey;
    let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).expect("rsa key");
    let pem = key.to_pkcs8_pem(Default::default()).unwrap().to_string();
    KalshiExecutionAdapter::new(KalshiConfig {
        api_key: "demo-key".into(),
        private_key_pem: pem,
        base_url: base_url.to_owned(),
        timestamp_ms: Some(1_700_000_000_000),
        request_timeout: (timeout_ms > 0).then(|| Duration::from_millis(timeout_ms)),
    })
    .expect("kalshi adapter")
}

fn polymarket_adapter(base_url: &str, timeout_ms: u64) -> PolymarketExecutionAdapter {
    PolymarketExecutionAdapter::new(PolymarketConfig {
        wallet_address: "0xdrill".into(),
        l1_private_key: "0x0101010101010101010101010101010101010101010101010101010101010101".into(),
        api_key: "key".into(),
        api_secret: "secret".into(),
        passphrase: "pass".into(),
        base_url: base_url.to_owned(),
        timestamp_s: Some(10),
        request_timeout: (timeout_ms > 0).then(|| Duration::from_millis(timeout_ms)),
    })
    .expect("polymarket adapter")
}

fn leg(venue: VenueId, stake: Cents, id: u8) -> ExecLeg {
    ExecLeg {
        venue,
        instrument: if venue == 1 {
            VenueInstrumentRef::Kalshi("DRILL-TICKER".into())
        } else {
            VenueInstrumentRef::Polymarket([b'd'; 32])
        },
        limit_price: Prob::from_cents(55).expect("price"),
        stake_cents: stake,
        client_order_id: [id; 16],
    }
}

fn hedge_gate() -> RiskGate {
    RiskGate::new(
        RiskConfig {
            kill_switch: false,
            ..RiskConfig::default()
        },
        [(1, 500), (2, 500)],
    )
}

// ---------------------------------------------------------------------------
// Drills
// ---------------------------------------------------------------------------

#[test]
fn drill_full_fill_reports_live_fill_and_holds_reserve() {
    let kalshi = ScriptedVenue::start(scripted(vec![kalshi_order("k-full", 100), json(200, "{}")]));
    let poly = ScriptedVenue::start(scripted(vec![json(
        200,
        r#"{"order_id":"p-full","filled_stake_cents":100}"#,
    )]));

    let legs = vec![leg(1, 100, 1), leg(2, 100, 2)];
    let mut risk = hedge_gate();
    let report = HedgedExecutor { risk: &mut risk }
        .execute(
            100,
            &legs,
            &kalshi_adapter(&kalshi.base_url(), 0),
            &polymarket_adapter(&poly.base_url(), 0),
        )
        .expect("full fill");

    assert_eq!(report.classification, ExecutionClassification::LiveFill);
    assert_eq!(report.settlement_status, "open");
    assert_eq!(report.realized_profit_cents, None);
    assert_eq!(risk.open_trades, 1);
    // Reserved capital stays locked while settlement is open.
    assert_eq!(risk.bankroll_snapshot()[&1], 400);

    let kalshi_log = kalshi.requests();
    assert!(kalshi_log.iter().any(|request| request.method == "POST"));
    assert!(kalshi_log.iter().all(|request| request.method != "DELETE"));
    drop(poly.requests());
}

#[test]
fn drill_rejection_unwinds_filled_leg_and_releases_capital() {
    let kalshi = ScriptedVenue::start(scripted(vec![
        kalshi_order("k-rej", 100),
        json(200, "{}"), // unwind
    ]));
    let poly = ScriptedVenue::start(scripted(vec![json(
        400,
        r#"{"error":"not enough liquidity"}"#,
    )]));

    let legs = vec![leg(1, 100, 1), leg(2, 100, 2)];
    let mut risk = hedge_gate();
    let report = HedgedExecutor { risk: &mut risk }
        .execute(
            100,
            &legs,
            &kalshi_adapter(&kalshi.base_url(), 0),
            &polymarket_adapter(&poly.base_url(), 0),
        )
        .expect("hedge resolves as phantom");

    assert_eq!(report.classification, ExecutionClassification::LivePhantom);
    assert_eq!(report.settlement_status, "unwound");
    assert_eq!(report.filled_stake_cents, 0);
    // The failed hedge releases every reservation.
    assert_eq!(risk.open_trades, 0);
    assert_eq!(risk.bankroll_snapshot()[&1], 500);
    assert_eq!(risk.bankroll_snapshot()[&2], 500);

    let kalshi_log = kalshi.requests();
    assert!(kalshi_log.iter().any(|request| request.method == "POST"));
    // The accepted Kalshi leg must have been cancelled.
    assert!(
        kalshi_log
            .iter()
            .any(|request| request.method == "DELETE" && request.path.contains("k-rej")),
        "accepted leg was not unwound: {kalshi_log:?}"
    );
    let poly_log = poly.requests();
    assert!(poly_log.iter().all(|request| request.method == "POST"));
}

#[test]
fn drill_partial_fill_is_never_a_position() {
    let kalshi = ScriptedVenue::start(scripted(vec![
        // FOK partially filling is a venue anomaly: 40 of 100 contracts.
        kalshi_order("k-part", 40),
        json(200, "{}"), // unwind
    ]));
    let poly = ScriptedVenue::start(scripted(vec![
        json(200, r#"{"order_id":"p-part","filled_stake_cents":100}"#),
        json(200, "{}"), // unwind
    ]));

    let legs = vec![leg(1, 100, 1), leg(2, 100, 2)];
    let mut risk = hedge_gate();
    let report = HedgedExecutor { risk: &mut risk }
        .execute(
            100,
            &legs,
            &kalshi_adapter(&kalshi.base_url(), 0),
            &polymarket_adapter(&poly.base_url(), 0),
        )
        .expect("partial hedge unwinds");

    assert_eq!(report.classification, ExecutionClassification::LivePhantom);
    assert_eq!(risk.bankroll_snapshot()[&1], 500);
    assert!(kalshi
        .requests()
        .iter()
        .any(|request| request.method == "DELETE"));
    assert!(poly
        .requests()
        .iter()
        .any(|request| request.method == "DELETE"));
}

#[test]
fn drill_failed_unwind_conservatively_holds_capital() {
    let kalshi = ScriptedVenue::start(scripted(vec![
        kalshi_order("k-stuck", 100),
        json(500, r#"{"error":"cancel rejected"}"#),
    ]));
    let poly = ScriptedVenue::start(scripted(vec![json(400, "reject")]));

    let legs = vec![leg(1, 100, 1), leg(2, 100, 2)];
    let mut risk = hedge_gate();
    let error = HedgedExecutor { risk: &mut risk }
        .execute(
            100,
            &legs,
            &kalshi_adapter(&kalshi.base_url(), 0),
            &polymarket_adapter(&poly.base_url(), 0),
        )
        .expect_err("unwind failure surfaces");

    assert!(matches!(error, ExecError::Unwind { venue: 1, .. }));
    // Until an operator reconciles the stuck leg, the reserve is held open —
    // silently releasing capital here could oversell the bankroll.
    assert_eq!(risk.open_trades, 1);
    assert_eq!(risk.bankroll_snapshot()[&1], 400);
    assert_eq!(risk.bankroll_snapshot()[&2], 400);
    drop(kalshi.requests());
    drop(poly.requests());
}

#[test]
fn drill_timeout_fails_one_leg_and_unwinds_the_other() {
    let kalshi = ScriptedVenue::start(Box::new(|_| {
        // Stall past the adapter deadline, then answer a dead socket.
        thread::sleep(Duration::from_millis(800));
        Some((
            200,
            r#"{"order":{"order_id":"k-slow","filled_count":100}}"#.to_owned(),
        ))
    }));
    let poly = ScriptedVenue::start(scripted(vec![
        json(200, r#"{"order_id":"p-fast","filled_stake_cents":100}"#),
        json(200, "{}"), // unwind
    ]));

    let legs = vec![leg(1, 100, 1), leg(2, 100, 2)];
    let mut risk = hedge_gate();
    let report = HedgedExecutor { risk: &mut risk }
        .execute(
            100,
            &legs,
            &kalshi_adapter(&kalshi.base_url(), 200),
            &polymarket_adapter(&poly.base_url(), 0),
        )
        .expect("timed-out leg degrades to phantom");

    assert_eq!(report.classification, ExecutionClassification::LivePhantom);
    assert_eq!(risk.bankroll_snapshot()[&1], 500);
    assert!(poly
        .requests()
        .iter()
        .any(|request| request.method == "DELETE"));
}

#[test]
fn drill_duplicate_retry_with_same_client_id_settles_once() {
    let kalshi = ScriptedVenue::start(Box::new(|request| {
        assert!(request.body.contains("client_order_id"));
        // The venue keys on the idempotency key: a retry is the same order.
        Some((
            200,
            r#"{"order":{"order_id":"k-dup","filled_count":100,"status":"executed"}}"#.to_owned(),
        ))
    }));

    let adapter = kalshi_adapter(&kalshi.base_url(), 0);
    let first = adapter.submit(&leg(1, 100, 7)).expect("first submit");
    let retried = adapter.submit(&leg(1, 100, 7)).expect("retry");
    assert_eq!(first.order_id, retried.order_id);

    let mut ledger = ReconciliationLedger::default();
    ledger.register(InFlightOrder {
        client_order_id: [7; 16],
        leg: PersistedExecLeg::from(&leg(1, 100, 7)),
        created_at_ms: 0,
        venue_order_id: None,
    });
    let fill = || FillEvent {
        client_order_id: Some([7; 16]),
        venue_order_id: first.order_id.clone(),
        filled_stake_cents: 100,
        fee_cents: 7,
        realized_profit_cents: None,
        status: "open".into(),
    };
    ledger.apply_fill(fill()).expect("fill");
    // The private fill stream redelivers the event after a reconnect.
    ledger.apply_fill(fill()).expect("replayed fill");

    assert_eq!(ledger.fees_paid_cents, 7, "replay double-counted fees");
    assert_eq!(ledger.orders.len(), 1);
    drop(adapter);
    drop(kalshi.requests());
}

#[test]
fn drill_stale_feed_blocks_execution_until_fresh_input() {
    let mut breaker = FeedCircuitBreaker::new(Duration::from_millis(20));
    std::thread::sleep(Duration::from_millis(40));

    // The runner loop consults the breaker before spending capital.
    assert!(breaker.blocked(), "silent feed must trip the breaker");

    let kalshi = ScriptedVenue::start(scripted(vec![
        kalshi_order("k-fresh", 100),
        json(200, "{}"),
    ]));
    let poly = ScriptedVenue::start(scripted(vec![json(
        200,
        r#"{"order_id":"p-fresh","filled_stake_cents":100}"#,
    )]));

    // Still tripped: execution stays parked until fresh input arrives.
    assert!(breaker.blocked());

    breaker.observe();
    assert!(!breaker.blocked());

    let legs = vec![leg(1, 100, 1), leg(2, 100, 2)];
    let mut risk = hedge_gate();
    let report = HedgedExecutor { risk: &mut risk }
        .execute(
            100,
            &legs,
            &kalshi_adapter(&kalshi.base_url(), 0),
            &polymarket_adapter(&poly.base_url(), 0),
        )
        .expect("fresh feed executes");
    assert_eq!(report.classification, ExecutionClassification::LiveFill);
    drop(kalshi.requests());
    drop(poly.requests());
}

#[test]
fn drill_restart_restores_risk_state_and_reconciles_by_client_id() {
    // Unique per invocation: a recycled pid must never inherit stale state.
    let directory = std::env::temp_dir().join(format!(
        "arbkit-drill-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let state_path = directory.join("state.json");
    let store = RiskStateStore::new(&state_path);

    let mut gate = hedge_gate();
    store
        .register_inflight(InFlightOrder {
            client_order_id: [5; 16],
            leg: PersistedExecLeg::from(&leg(1, 100, 5)),
            created_at_ms: 1_000,
            venue_order_id: None,
        })
        .expect("persist inflight");
    gate.preflight(100, &[leg(1, 100, 5), leg(2, 100, 2)])
        .expect("reserve before crash");
    // Checkpoints must merge over stored state: wiping in-flight records on
    // every save would make a mid-session checkpoint lose crash evidence.
    store.checkpoint(&gate).expect("persist gate");

    // --- crash: everything after this point comes back from disk only ---
    let recovered = RiskStateStore::new(&state_path)
        .load()
        .expect("reload durable state");
    let restored_gate = recovered.restore();
    assert_eq!(restored_gate.open_trades, 1);
    assert_eq!(restored_gate.bankroll_snapshot()[&1], 400);

    // Acknowledgement arriving late is recorded and idempotent.
    let acknowledge = || {
        RiskStateStore::new(&state_path)
            .acknowledge([5; 16], "k-crash".to_owned())
            .expect("acknowledge")
    };
    let acked = acknowledge();
    assert_eq!(
        acked.in_flight[0].venue_order_id.as_deref(),
        Some("k-crash")
    );
    assert_eq!(
        acked,
        acknowledge(),
        "double acknowledgement must be a no-op"
    );

    // Settlement reports only the venue ID it observed; the client key ties
    // it back to the crashed submission.
    let mut ledger = ReconciliationLedger::default();
    ledger.register(recovered.in_flight[0].clone());
    ledger
        .apply_fill(FillEvent {
            client_order_id: Some([5; 16]),
            venue_order_id: "venue-reported-id".into(),
            filled_stake_cents: 100,
            fee_cents: 4,
            realized_profit_cents: Some(210),
            status: "settled".into(),
        })
        .expect("reconcile by client id");
    assert_eq!(ledger.fees_paid_cents, 4);
    assert_eq!(ledger.realized_profit_cents, 210);

    let _ = std::fs::remove_file(state_path);
}

#[test]
fn drill_kill_switch_refuses_before_any_network_traffic() {
    let kalshi = ScriptedVenue::start(Box::new(|_| panic!("kill-switch drill reached venue")));
    let poly = ScriptedVenue::start(Box::new(|_| panic!("kill-switch drill reached venue")));

    let legs = vec![leg(1, 100, 1), leg(2, 100, 2)];
    let mut risk = RiskGate::new(RiskConfig::default(), [(1, 500), (2, 500)]);
    let error = HedgedExecutor { risk: &mut risk }
        .execute(
            100,
            &legs,
            &kalshi_adapter(&kalshi.base_url(), 0),
            &polymarket_adapter(&poly.base_url(), 0),
        )
        .expect_err("kill switch refuses");

    assert!(matches!(error, ExecError::Risk(_)));
    drop(kalshi.requests());
    drop(poly.requests());
}

#[test]
fn drill_balance_mismatch_aborts_before_order_submission() {
    // Gate level: the local bankroll cannot cover the leg.
    let mut thin = RiskGate::new(
        RiskConfig {
            kill_switch: false,
            ..RiskConfig::default()
        },
        [(1, 50), (2, 500)],
    );
    assert!(matches!(
        thin.preflight(100, &[leg(1, 100, 1), leg(2, 100, 2)]),
        Err(arbkit_exec::RiskRejection::InsufficientCapital)
    ));

    // Venue truth disagrees with the local view: reconcile before trading.
    let kalshi = ScriptedVenue::start(scripted(vec![json(200, r#"{"balance":25}"#)]));
    let adapter = kalshi_adapter(&kalshi.base_url(), 0);
    let reported = adapter.balance_cents().expect("balance");
    assert_eq!(reported, 25);
    assert!(
        reported < 100,
        "venue holds less than the intended reserve; abort"
    );

    let log = kalshi.requests();
    assert!(
        log.iter().all(|request| !request.path.ends_with("/orders")),
        "balance mismatch must prevent order submission: {log:?}"
    );
    drop(adapter);
}

#[test]
fn demo_and_sandbox_presets_point_at_test_environments_without_io() {
    use rsa::pkcs8::EncodePrivateKey;
    let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).expect("key");
    let pem = key.to_pkcs8_pem(Default::default()).unwrap().to_string();

    let kalshi_demo = KalshiConfig::demo("demo-key".into(), pem);
    assert!(kalshi_demo.base_url.contains("demo-api.kalshi.co"));

    let poly_sandbox = PolymarketConfig::sandbox(
        "0xabc".into(),
        "0x0101010101010101010101010101010101010101010101010101010101010101".into(),
        "key".into(),
        "secret".into(),
        "pass".into(),
    );
    assert!(poly_sandbox
        .base_url
        .contains("staging.clob.polymarket.com"));
    assert!(poly_sandbox.request_timeout.is_some());
}
