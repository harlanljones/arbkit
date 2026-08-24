//! Kalshi event and market ticker parser.
//!
//! Live event parts are stamped season-year first:
//! `KXMLBGAME-26AUG241840TBDET-TB` reads `[YY=26][MMM=AUG][DD=24][HHMM=1840]`
//! plus variable-length team codes — an Aug 24, 2026 game starting 18:40.
//! Dates are canonicalized to `YYYY-MM-DD`.

use arbkit_core::{Line, MarketKind};

use crate::error::{MatchError, Result};
use crate::team::{lookup_team, CanonicalTeam, Matchup, Sport};

/// A parsed Kalshi market ticker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KalshiTicker {
    /// Sport or league.
    pub sport: Sport,
    /// Canonical game date (`YYYY-MM-DD`, US Eastern).
    pub date_code: String,
    /// Matchup (home and away teams).
    pub matchup: Matchup,
    /// The market kind (Moneyline, Spread, Total) if determinable from ticker.
    pub market_kind: Option<MarketKind>,
    /// Specific outcome target team if applicable (e.g. "BOS").
    pub target_team: Option<&'static CanonicalTeam>,
    /// For totals, whether this ticker represents the Over (`true`) or Under (`false`).
    pub is_over: Option<bool>,
}

/// Decode the leading `[YY][MMM][D]{1,2}[HHMM]` of an event part.
///
/// Returns the canonical `YYYY-MM-DD` date and the remaining team-code tail.
/// The day may be one or two digits; the split is validated against a legal
/// calendar day and a legal `HHMM` clock time, so `241840TBDET` decodes as
/// day 24 at 18:40 while `71905KCTOR` decodes as day 7 at 19:05. Ambiguous
/// shapes that admit no such reading are malformed rather than guessed.
fn parse_event_datetime<'a>(ticker: &str, rest: &'a str) -> Result<(String, &'a str)> {
    const MALFORMED_TAIL: &str =
        "event part must open with [YY][MMM][D]{1,2}[HHMM] before the team codes";

    let bad = || MatchError::MalformedTicker(ticker.to_string(), MALFORMED_TAIL);

    let b = rest.as_bytes();
    if b.len() < 10
        || !b[..2].iter().all(u8::is_ascii_digit)
        || !b[2..5].iter().all(u8::is_ascii_uppercase)
    {
        return Err(bad());
    }
    let year = match rest[..2].parse::<u8>() {
        Ok(y) if y >= 20 => y as u32,
        _ => return Err(bad()),
    };
    let month = match &rest[2..5] {
        "JAN" => 1,
        "FEB" => 2,
        "MAR" => 3,
        "APR" => 4,
        "MAY" => 5,
        "JUN" => 6,
        "JUL" => 7,
        "AUG" => 8,
        "SEP" => 9,
        "OCT" => 10,
        "NOV" => 11,
        "DEC" => 12,
        _ => return Err(bad()),
    };

    let tail = &rest[5..];
    let tb = tail.as_bytes();
    for take in [2usize, 1] {
        if tb.len() < take + 4 {
            continue;
        }
        if !tb[..take].iter().all(u8::is_ascii_digit) {
            continue;
        }
        let day: u32 = match tail[..take].parse() {
            Ok(d) if (1..=31).contains(&d) => d,
            _ => continue,
        };
        let time = &tail[take..take + 4];
        if !time.bytes().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let (hh, mm) = (
            time[..2].parse::<u32>().unwrap_or(99),
            time[2..].parse::<u32>().unwrap_or(99),
        );
        if hh > 23 || mm > 59 {
            continue;
        }
        let date = format!("{:04}-{:02}-{:02}", 2000 + year, month, day);
        return Ok((date, &tail[take + 4..]));
    }
    Err(bad())
}

/// Parse a Kalshi ticker string into structured event and market information.
pub fn parse_kalshi_ticker(ticker: &str) -> Result<KalshiTicker> {
    let parts: Vec<&str> = ticker.split('-').collect();
    if parts.len() < 2 {
        return Err(MatchError::MalformedTicker(
            ticker.to_string(),
            "expected at least series and event parts separated by '-'",
        ));
    }

    let series_part = parts[0];
    let event_part = parts[1];
    let outcome_part = parts.get(2).copied();

    // 1. Parse series part (e.g., "KXNBAGAME", "KXNBASPREAD", "KXNBATOTAL")
    let (sport, kind_hint) = parse_series_prefix(series_part)?;

    // 2. Parse event part (e.g. "26AUG241840TBDET"): season-year stamp,
    // month, day, four-digit start time, then two variable-length codes.
    let (date_code, teams_str) = parse_event_datetime(ticker, event_part)?;

    let (away_team, home_team) = resolve_team_pair(teams_str, ticker, sport)?;
    let matchup = Matchup::new(home_team, away_team);

    // 3. Parse outcome part if present
    let mut market_kind = None;
    let mut target_team = None;
    let mut is_over = None;

    match kind_hint {
        KalshiKindHint::Game => {
            market_kind = Some(MarketKind::Moneyline);
            if let Some(target) = outcome_part {
                target_team = Some(lookup_team(target, Some(sport))?);
            }
        }
        KalshiKindHint::Spread => {
            if let Some(outcome) = outcome_part {
                let (team, line) = parse_spread_outcome(outcome, sport)?;
                target_team = Some(team);
                // Normalized to home team view:
                // If the target team is home, line is as quoted.
                // If target team is away, spread on home is mirrored.
                let normalized_line = if team == home_team {
                    line
                } else if team == away_team {
                    line.mirrored()
                } else {
                    return Err(MatchError::UnrecognizedTeam(team.code.to_string()));
                };
                market_kind = Some(MarketKind::Spread(normalized_line));
            }
        }
        KalshiKindHint::Total => {
            if let Some(outcome) = outcome_part {
                let (line, over) = parse_total_outcome(outcome)?;
                market_kind = Some(MarketKind::Total(line));
                is_over = Some(over);
            }
        }
        KalshiKindHint::Unknown => {}
    }

    Ok(KalshiTicker {
        sport,
        date_code,
        matchup,
        market_kind,
        target_team,
        is_over,
    })
}

enum KalshiKindHint {
    Game,
    Spread,
    Total,
    Unknown,
}

/// Split the concatenated team-code tail of an event part into away and home.
///
/// Kalshi codes run two to three letters ("TBDET", "CHCAZ", "BOSNYY"), so the
/// split point is not positional. Every split point is tried against the
/// sport's alias table and exactly one split must resolve both halves to two
/// distinct clubs; zero or multiple valid splits are malformed rather than
/// guessed.
fn resolve_team_pair(
    teams_str: &str,
    ticker: &str,
    sport: Sport,
) -> Result<(&'static CanonicalTeam, &'static CanonicalTeam)> {
    let malformed = |why: &'static str| MatchError::MalformedTicker(ticker.to_string(), why);
    if !(4..=8).contains(&teams_str.len()) {
        return Err(malformed(
            "team-code tail must hold two codes of two to four letters",
        ));
    }

    let mut found: Option<(&'static CanonicalTeam, &'static CanonicalTeam)> = None;
    for i in 2..teams_str.len() - 1 {
        let (Ok(away), Ok(home)) = (
            lookup_team(&teams_str[..i], Some(sport)),
            lookup_team(&teams_str[i..], Some(sport)),
        ) else {
            continue;
        };
        if away == home {
            continue;
        }
        if found.is_some() {
            return Err(malformed("team-code split is ambiguous"));
        }
        found = Some((away, home));
    }

    found.ok_or_else(|| malformed("team codes do not resolve to two distinct clubs"))
}

fn parse_series_prefix(prefix: &str) -> Result<(Sport, KalshiKindHint)> {
    let sport = if prefix.starts_with("KXNBA") {
        Sport::Nba
    } else if prefix.starts_with("KXNFL") {
        Sport::Nfl
    } else if prefix.starts_with("KXMLB") {
        Sport::Mlb
    } else if prefix.starts_with("KXNHL") {
        Sport::Nhl
    } else {
        Sport::Other
    };

    let kind_hint = if prefix.contains("GAME") {
        KalshiKindHint::Game
    } else if prefix.contains("SPREAD") {
        KalshiKindHint::Spread
    } else if prefix.contains("TOTAL") {
        KalshiKindHint::Total
    } else {
        KalshiKindHint::Unknown
    };

    Ok((sport, kind_hint))
}

fn parse_spread_outcome(outcome: &str, sport: Sport) -> Result<(&'static CanonicalTeam, Line)> {
    // Team codes run two to three letters ("TEX4", "CWS2", "TB15"), so the
    // code is the longest resolvable prefix; what follows must be line digits.
    for take in (2..=3).rev() {
        if outcome.len() <= take {
            continue;
        }
        if let Ok(team) = lookup_team(&outcome[..take], Some(sport)) {
            let hundredths = parse_points_to_hundredths(&outcome[take..])?;
            return Ok((team, Line::from_hundredths(hundredths)));
        }
    }

    Err(MatchError::MalformedTicker(
        outcome.to_string(),
        "spread outcome must start with a resolvable team code followed by line digits",
    ))
}

fn parse_total_outcome(outcome: &str) -> Result<(Line, bool)> {
    if outcome.is_empty() {
        return Err(MatchError::MalformedTicker(
            outcome.to_string(),
            "empty total outcome string",
        ));
    }

    let is_over = if outcome.ends_with('O') || outcome.ends_with('o') {
        true
    } else if outcome.ends_with('U') || outcome.ends_with('u') {
        false
    } else if outcome.starts_with('O') || outcome.starts_with('o') {
        true
    } else if outcome.starts_with('U') || outcome.starts_with('u') {
        false
    } else {
        return Err(MatchError::MalformedTicker(
            outcome.to_string(),
            "total outcome must end or start with 'O' (Over) or 'U' (Under)",
        ));
    };

    let digits_part = outcome
        .trim_matches(|c: char| c == 'O' || c == 'o' || c == 'U' || c == 'u')
        .trim();

    // e.g. "2205" -> 220.5 -> 22050 hundredths
    let hundredths = parse_points_to_hundredths(digits_part)?;
    Ok((Line::from_hundredths(hundredths), is_over))
}

fn parse_points_to_hundredths(s: &str) -> Result<i32> {
    if let Ok(float_val) = s.parse::<f64>() {
        // If integer string without dot like "35" for 3.5 or "2205" for 220.5:
        if !s.contains('.') {
            // If length is 2 (e.g. 35 -> 3.5)
            // Or length is 4 (e.g. 2205 -> 220.5)
            // Standard Kalshi convention: last digit is tenths
            let int_val: i32 = s.parse().map_err(|_| {
                MatchError::MalformedTicker(s.to_string(), "failed to parse point number")
            })?;
            return Ok(int_val * 10);
        }
        let rounded = (float_val * 100.0).round() as i32;
        Ok(rounded)
    } else {
        Err(MatchError::MalformedTicker(
            s.to_string(),
            "invalid line number format",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kalshi_game_moneyline_ticker() {
        let ticker = "KXNBAGAME-26AUG181930BOSLAL-BOS";
        let parsed = parse_kalshi_ticker(ticker).unwrap();

        assert_eq!(parsed.sport, Sport::Nba);
        assert_eq!(parsed.date_code, "2026-08-18");
        assert_eq!(parsed.matchup.away.code, "BOS");
        assert_eq!(parsed.matchup.home.code, "LAL");
        assert_eq!(parsed.market_kind, Some(MarketKind::Moneyline));
        assert_eq!(parsed.target_team.unwrap().code, "BOS");
    }

    #[test]
    fn event_datetime_decodes_year_first() {
        // Live shape captured from the public markets API, Aug 2026: season
        // year, month, DAY, then start time. The pre-live fixture grammar
        // read this as DDMMMYY and only survived because its examples were
        // palindromic in the two numbers ("26AUG26"); real slates are not.
        let parsed = parse_kalshi_ticker("KXMLBGAME-26AUG241840TBDET-TB").unwrap();
        assert_eq!(parsed.date_code, "2026-08-24");

        let parsed = parse_kalshi_ticker("KXMLBGAME-26AUG251840TBDET-TB").unwrap();
        assert_eq!(parsed.date_code, "2026-08-25");

        let parsed = parse_kalshi_ticker("KXMLBGAME-26AUG261310TBDET-TB").unwrap();
        assert_eq!(parsed.date_code, "2026-08-26");

        // Single-digit day: "71905" is day 7 at 19:05, not day 71.
        let parsed = parse_kalshi_ticker("KXMLBGAME-26AUG71905KCTOR-KC").unwrap();
        assert_eq!(parsed.date_code, "2026-08-07");
        assert_eq!(parsed.matchup.away.code, "KC");

        // Impossible clocks and days are malformed, never reinterpreted.
        for part in [
            "26AUG242540TBDET", // hour 25
            "26AUG241875TBDET", // minute 75
            "26AUG991840TBDET", // day 99
            "26XXX241840TBDET", // unknown month
            "2AAUG241840TBDET", // non-numeric year
            "26AUG26XXXXBOSNYY",
        ] {
            assert!(
                parse_kalshi_ticker(&format!("KXMLBGAME-{part}-TB")).is_err(),
                "{part} must be rejected"
            );
        }
    }

    #[test]
    fn live_mlb_tickers_split_variable_length_codes() {
        // Shapes captured from the public markets API in Aug 2026: two-letter
        // and three-letter codes concatenated with a 4-digit start time.
        let parsed = parse_kalshi_ticker("KXMLBGAME-26AUG241840TBDET-TB").unwrap();
        assert_eq!(parsed.matchup.away.code, "TB");
        assert_eq!(parsed.matchup.home.code, "DET");
        assert_eq!(parsed.target_team.unwrap().code, "TB");

        let parsed = parse_kalshi_ticker("KXMLBGAME-26AUG242140CHCAZ-AZ").unwrap();
        assert_eq!(parsed.matchup.away.code, "CHC");
        assert_eq!(parsed.matchup.home.code, "AZ");
        assert_eq!(parsed.target_team.unwrap().code, "AZ");

        let parsed = parse_kalshi_ticker("KXMLBGAME-26AUG242140MINATH-ATH").unwrap();
        assert_eq!(parsed.matchup.away.code, "MIN");
        assert_eq!(parsed.matchup.home.code, "ATH");

        // Three-letter pairs keep parsing through the same path.
        let parsed = parse_kalshi_ticker("KXMLBGAME-26AUG261905BOSNYY-NYY").unwrap();
        assert_eq!(parsed.matchup.home.code, "NYY");
    }

    #[test]
    fn live_mlb_spread_outcomes_match_two_letter_codes() {
        let parsed = parse_kalshi_ticker("KXMLBSPREAD-26AUG241840TBDET-TB15").unwrap();
        assert_eq!(parsed.target_team.unwrap().code, "TB");
        // TB is away, so its quoted line is mirrored into home view.
        assert_eq!(
            parsed.market_kind,
            Some(MarketKind::Spread(Line::from_hundredths(150).mirrored()))
        );

        // A three-letter code keeps working through the longest-prefix path.
        let parsed = parse_kalshi_ticker("KXMLBSPREAD-26AUG241940TEXCWS-CWS4").unwrap();
        assert_eq!(parsed.target_team.unwrap().code, "CWS");
    }

    #[test]
    fn unresolvable_team_tail_is_never_guessed() {
        assert!(parse_kalshi_ticker("KXMLBGAME-26AUG18ZZZZZZ-BOS").is_err());
        assert!(parse_kalshi_ticker("KXMLBGAME-26AUG18BOS-BOS").is_err());
        assert!(parse_kalshi_ticker("KXMLBGAME-26AUG182105BOSNYYEXTRA-BOS").is_err());
    }

    #[test]
    fn parse_kalshi_spread_ticker() {
        let ticker = "KXNBASPREAD-26AUG181930BOSLAL-LAL35";
        let parsed = parse_kalshi_ticker(ticker).unwrap();

        assert_eq!(parsed.sport, Sport::Nba);
        assert_eq!(parsed.matchup.home.code, "LAL");
        assert_eq!(parsed.matchup.away.code, "BOS");
        assert_eq!(parsed.target_team.unwrap().code, "LAL");
        // LAL is home, so LAL +3.5 or 3.5 gives Line(350)
        assert_eq!(
            parsed.market_kind,
            Some(MarketKind::Spread(Line::from_hundredths(350)))
        );
    }

    #[test]
    fn parse_kalshi_total_ticker() {
        let ticker = "KXNBATOTAL-26AUG181930BOSLAL-2205O";
        let parsed = parse_kalshi_ticker(ticker).unwrap();

        assert_eq!(parsed.sport, Sport::Nba);
        assert_eq!(parsed.is_over, Some(true));
        assert_eq!(
            parsed.market_kind,
            Some(MarketKind::Total(Line::from_hundredths(22050)))
        );
    }
}
