//! Canonical team identification, aliasing, and matchup parsing.
//!
//! Different venues identify the same sports team with varying names, abbreviations,
//! and city prefixes (e.g., "Boston Celtics", "BOS", "Celtics", "Boston", "BOS Celtics").
//! This module normalizes diverse input strings into canonical, static team representations
//! and parses matchup strings into home and away teams.

use crate::error::{MatchError, Result};

/// A sport or league category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sport {
    /// National Basketball Association.
    Nba,
    /// National Football League.
    Nfl,
    /// Major League Baseball.
    Mlb,
    /// National Hockey League.
    Nhl,
    /// Association Football (Soccer).
    Soccer,
    /// Other sport or unspecified category.
    Other,
}

/// A canonical sports team definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanonicalTeam {
    /// Sport or league this team belongs to.
    pub sport: Sport,
    /// Standard three-character or short ticker code (e.g. "BOS", "LAL").
    pub code: &'static str,
    /// Full franchise name (e.g. "Boston Celtics", "Los Angeles Lakers").
    pub full_name: &'static str,
    /// Geographic city or region name (e.g. "Boston", "Los Angeles").
    pub city: &'static str,
    /// Team mascot/nickname (e.g. "Celtics", "Lakers").
    pub mascot: &'static str,
}

impl CanonicalTeam {
    /// Create a new canonical team definition.
    pub const fn new(
        sport: Sport,
        code: &'static str,
        full_name: &'static str,
        city: &'static str,
        mascot: &'static str,
    ) -> Self {
        Self {
            sport,
            code,
            full_name,
            city,
            mascot,
        }
    }
}

/// A matchup consisting of a canonical home team and an away team.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Matchup {
    /// The home team hosting the event.
    pub home: &'static CanonicalTeam,
    /// The visiting away team.
    pub away: &'static CanonicalTeam,
}

impl Matchup {
    /// Create a new matchup with specified home and away teams.
    pub const fn new(home: &'static CanonicalTeam, away: &'static CanonicalTeam) -> Self {
        Self { home, away }
    }

    /// Reverse home and away roles.
    pub const fn reversed(self) -> Self {
        Self {
            home: self.away,
            away: self.home,
        }
    }
}

// NBA Teams
const NBA_ATL: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "ATL", "Atlanta Hawks", "Atlanta", "Hawks");
const NBA_BOS: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "BOS", "Boston Celtics", "Boston", "Celtics");
const NBA_BKN: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "BKN", "Brooklyn Nets", "Brooklyn", "Nets");
const NBA_CHA: CanonicalTeam = CanonicalTeam::new(
    Sport::Nba,
    "CHA",
    "Charlotte Hornets",
    "Charlotte",
    "Hornets",
);
const NBA_CHI: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "CHI", "Chicago Bulls", "Chicago", "Bulls");
const NBA_CLE: CanonicalTeam = CanonicalTeam::new(
    Sport::Nba,
    "CLE",
    "Cleveland Cavaliers",
    "Cleveland",
    "Cavaliers",
);
const NBA_DAL: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "DAL", "Dallas Mavericks", "Dallas", "Mavericks");
const NBA_DEN: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "DEN", "Denver Nuggets", "Denver", "Nuggets");
const NBA_DET: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "DET", "Detroit Pistons", "Detroit", "Pistons");
const NBA_GSW: CanonicalTeam = CanonicalTeam::new(
    Sport::Nba,
    "GSW",
    "Golden State Warriors",
    "Golden State",
    "Warriors",
);
const NBA_HOU: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "HOU", "Houston Rockets", "Houston", "Rockets");
const NBA_IND: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "IND", "Indiana Pacers", "Indiana", "Pacers");
const NBA_LAC: CanonicalTeam = CanonicalTeam::new(
    Sport::Nba,
    "LAC",
    "Los Angeles Clippers",
    "Los Angeles",
    "Clippers",
);
const NBA_LAL: CanonicalTeam = CanonicalTeam::new(
    Sport::Nba,
    "LAL",
    "Los Angeles Lakers",
    "Los Angeles",
    "Lakers",
);
const NBA_MEM: CanonicalTeam = CanonicalTeam::new(
    Sport::Nba,
    "MEM",
    "Memphis Grizzlies",
    "Memphis",
    "Grizzlies",
);
const NBA_MIA: CanonicalTeam = CanonicalTeam::new(Sport::Nba, "MIA", "Miami Heat", "Miami", "Heat");
const NBA_MIL: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "MIL", "Milwaukee Bucks", "Milwaukee", "Bucks");
const NBA_MIN: CanonicalTeam = CanonicalTeam::new(
    Sport::Nba,
    "MIN",
    "Minnesota Timberwolves",
    "Minnesota",
    "Timberwolves",
);
const NBA_NOP: CanonicalTeam = CanonicalTeam::new(
    Sport::Nba,
    "NOP",
    "New Orleans Pelicans",
    "New Orleans",
    "Pelicans",
);
const NBA_NYK: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "NYK", "New York Knicks", "New York", "Knicks");
const NBA_OKC: CanonicalTeam = CanonicalTeam::new(
    Sport::Nba,
    "OKC",
    "Oklahoma City Thunder",
    "Oklahoma City",
    "Thunder",
);
const NBA_ORL: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "ORL", "Orlando Magic", "Orlando", "Magic");
const NBA_PHI: CanonicalTeam = CanonicalTeam::new(
    Sport::Nba,
    "PHI",
    "Philadelphia 76ers",
    "Philadelphia",
    "76ers",
);
const NBA_PHX: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "PHX", "Phoenix Suns", "Phoenix", "Suns");
const NBA_POR: CanonicalTeam = CanonicalTeam::new(
    Sport::Nba,
    "POR",
    "Portland Trail Blazers",
    "Portland",
    "Trail Blazers",
);
const NBA_SAC: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "SAC", "Sacramento Kings", "Sacramento", "Kings");
const NBA_SAS: CanonicalTeam = CanonicalTeam::new(
    Sport::Nba,
    "SAS",
    "San Antonio Spurs",
    "San Antonio",
    "Spurs",
);
const NBA_TOR: CanonicalTeam =
    CanonicalTeam::new(Sport::Nba, "TOR", "Toronto Raptors", "Toronto", "Raptors");
const NBA_UTA: CanonicalTeam = CanonicalTeam::new(Sport::Nba, "UTA", "Utah Jazz", "Utah", "Jazz");
const NBA_WAS: CanonicalTeam = CanonicalTeam::new(
    Sport::Nba,
    "WAS",
    "Washington Wizards",
    "Washington",
    "Wizards",
);

// NFL Teams (Selected common representatives)
const NFL_KC: CanonicalTeam = CanonicalTeam::new(
    Sport::Nfl,
    "KC",
    "Kansas City Chiefs",
    "Kansas City",
    "Chiefs",
);
const NFL_SF: CanonicalTeam = CanonicalTeam::new(
    Sport::Nfl,
    "SF",
    "San Francisco 49ers",
    "San Francisco",
    "49ers",
);
const NFL_BAL: CanonicalTeam =
    CanonicalTeam::new(Sport::Nfl, "BAL", "Baltimore Ravens", "Baltimore", "Ravens");
const NFL_BUF: CanonicalTeam =
    CanonicalTeam::new(Sport::Nfl, "BUF", "Buffalo Bills", "Buffalo", "Bills");
const NFL_DAL: CanonicalTeam =
    CanonicalTeam::new(Sport::Nfl, "DAL", "Dallas Cowboys", "Dallas", "Cowboys");
const NFL_GB: CanonicalTeam = CanonicalTeam::new(
    Sport::Nfl,
    "GB",
    "Green Bay Packers",
    "Green Bay",
    "Packers",
);
const NFL_NE: CanonicalTeam = CanonicalTeam::new(
    Sport::Nfl,
    "NE",
    "New England Patriots",
    "New England",
    "Patriots",
);
const NFL_PHI: CanonicalTeam = CanonicalTeam::new(
    Sport::Nfl,
    "PHI",
    "Philadelphia Eagles",
    "Philadelphia",
    "Eagles",
);

// MLB Teams (complete 30-team roster)
//
// Codes are the ones Kalshi actually embeds in live KXMLB tickers
// (`KXMLBGAME-26AUG241840TBDET-TB`), captured from the public markets API —
// including two-letter codes (AZ, KC, SD, SF, TB) that a fixed 3+3 split can
// never parse. Full names are Polymarket's exact outcome labels.
const MLB_AZ: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "AZ",
    "Arizona Diamondbacks",
    "Arizona",
    "Diamondbacks",
);
const MLB_ATL: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "ATL", "Atlanta Braves", "Atlanta", "Braves");
const MLB_ATH: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "ATH", "Athletics", "Sacramento", "Athletics");
const MLB_BAL: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "BAL",
    "Baltimore Orioles",
    "Baltimore",
    "Orioles",
);
const MLB_BOS: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "BOS", "Boston Red Sox", "Boston", "Red Sox");
const MLB_CHC: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "CHC", "Chicago Cubs", "Chicago", "Cubs");
const MLB_CWS: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "CWS",
    "Chicago White Sox",
    "Chicago",
    "White Sox",
);
const MLB_CIN: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "CIN", "Cincinnati Reds", "Cincinnati", "Reds");
const MLB_CLE: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "CLE",
    "Cleveland Guardians",
    "Cleveland",
    "Guardians",
);
const MLB_COL: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "COL", "Colorado Rockies", "Colorado", "Rockies");
const MLB_DET: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "DET", "Detroit Tigers", "Detroit", "Tigers");
const MLB_HOU: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "HOU", "Houston Astros", "Houston", "Astros");
const MLB_KC: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "KC",
    "Kansas City Royals",
    "Kansas City",
    "Royals",
);
const MLB_LAA: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "LAA",
    "Los Angeles Angels",
    "Los Angeles",
    "Angels",
);
const MLB_LAD: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "LAD",
    "Los Angeles Dodgers",
    "Los Angeles",
    "Dodgers",
);
const MLB_MIA: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "MIA", "Miami Marlins", "Miami", "Marlins");
const MLB_MIL: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "MIL",
    "Milwaukee Brewers",
    "Milwaukee",
    "Brewers",
);
const MLB_MIN: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "MIN", "Minnesota Twins", "Minnesota", "Twins");
const MLB_NYM: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "NYM", "New York Mets", "New York", "Mets");
const MLB_NYY: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "NYY", "New York Yankees", "New York", "Yankees");
const MLB_PHI: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "PHI",
    "Philadelphia Phillies",
    "Philadelphia",
    "Phillies",
);
const MLB_PIT: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "PIT",
    "Pittsburgh Pirates",
    "Pittsburgh",
    "Pirates",
);
const MLB_SD: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "SD", "San Diego Padres", "San Diego", "Padres");
const MLB_SEA: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "SEA", "Seattle Mariners", "Seattle", "Mariners");
const MLB_SF: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "SF",
    "San Francisco Giants",
    "San Francisco",
    "Giants",
);
const MLB_STL: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "STL",
    "St. Louis Cardinals",
    "St. Louis",
    "Cardinals",
);
const MLB_TB: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "TB", "Tampa Bay Rays", "Tampa Bay", "Rays");
const MLB_TEX: CanonicalTeam =
    CanonicalTeam::new(Sport::Mlb, "TEX", "Texas Rangers", "Texas", "Rangers");
const MLB_TOR: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "TOR",
    "Toronto Blue Jays",
    "Toronto",
    "Blue Jays",
);
const MLB_WSH: CanonicalTeam = CanonicalTeam::new(
    Sport::Mlb,
    "WSH",
    "Washington Nationals",
    "Washington",
    "Nationals",
);

// NHL Teams (Selected common representatives)
const NHL_BOS: CanonicalTeam =
    CanonicalTeam::new(Sport::Nhl, "BOS", "Boston Bruins", "Boston", "Bruins");
const NHL_TOR: CanonicalTeam = CanonicalTeam::new(
    Sport::Nhl,
    "TOR",
    "Toronto Maple Leafs",
    "Toronto",
    "Maple Leafs",
);
const NHL_EDM: CanonicalTeam =
    CanonicalTeam::new(Sport::Nhl, "EDM", "Edmonton Oilers", "Edmonton", "Oilers");

struct AliasMapping {
    alias: &'static str,
    team: &'static CanonicalTeam,
}

// Pre-compiled aliases table
static ALIASES: &[AliasMapping] = &[
    // NBA
    AliasMapping {
        alias: "atl",
        team: &NBA_ATL,
    },
    AliasMapping {
        alias: "atlanta",
        team: &NBA_ATL,
    },
    AliasMapping {
        alias: "hawks",
        team: &NBA_ATL,
    },
    AliasMapping {
        alias: "atlantahawks",
        team: &NBA_ATL,
    },
    AliasMapping {
        alias: "bos",
        team: &NBA_BOS,
    },
    AliasMapping {
        alias: "boston",
        team: &NBA_BOS,
    },
    AliasMapping {
        alias: "celtics",
        team: &NBA_BOS,
    },
    AliasMapping {
        alias: "bostonceltics",
        team: &NBA_BOS,
    },
    AliasMapping {
        alias: "bosceltics",
        team: &NBA_BOS,
    },
    AliasMapping {
        alias: "bkn",
        team: &NBA_BKN,
    },
    AliasMapping {
        alias: "brooklyn",
        team: &NBA_BKN,
    },
    AliasMapping {
        alias: "nets",
        team: &NBA_BKN,
    },
    AliasMapping {
        alias: "brooklynnets",
        team: &NBA_BKN,
    },
    AliasMapping {
        alias: "bknnets",
        team: &NBA_BKN,
    },
    AliasMapping {
        alias: "cha",
        team: &NBA_CHA,
    },
    AliasMapping {
        alias: "charlotte",
        team: &NBA_CHA,
    },
    AliasMapping {
        alias: "hornets",
        team: &NBA_CHA,
    },
    AliasMapping {
        alias: "charlottehornets",
        team: &NBA_CHA,
    },
    AliasMapping {
        alias: "chi",
        team: &NBA_CHI,
    },
    AliasMapping {
        alias: "chicago",
        team: &NBA_CHI,
    },
    AliasMapping {
        alias: "bulls",
        team: &NBA_CHI,
    },
    AliasMapping {
        alias: "chicagobulls",
        team: &NBA_CHI,
    },
    AliasMapping {
        alias: "cle",
        team: &NBA_CLE,
    },
    AliasMapping {
        alias: "cleveland",
        team: &NBA_CLE,
    },
    AliasMapping {
        alias: "cavaliers",
        team: &NBA_CLE,
    },
    AliasMapping {
        alias: "cavs",
        team: &NBA_CLE,
    },
    AliasMapping {
        alias: "clevelandcavaliers",
        team: &NBA_CLE,
    },
    AliasMapping {
        alias: "dal",
        team: &NBA_DAL,
    },
    AliasMapping {
        alias: "dallas",
        team: &NBA_DAL,
    },
    AliasMapping {
        alias: "mavericks",
        team: &NBA_DAL,
    },
    AliasMapping {
        alias: "mavs",
        team: &NBA_DAL,
    },
    AliasMapping {
        alias: "dallasmavericks",
        team: &NBA_DAL,
    },
    AliasMapping {
        alias: "den",
        team: &NBA_DEN,
    },
    AliasMapping {
        alias: "denver",
        team: &NBA_DEN,
    },
    AliasMapping {
        alias: "nuggets",
        team: &NBA_DEN,
    },
    AliasMapping {
        alias: "denvernuggets",
        team: &NBA_DEN,
    },
    AliasMapping {
        alias: "det",
        team: &NBA_DET,
    },
    AliasMapping {
        alias: "detroit",
        team: &NBA_DET,
    },
    AliasMapping {
        alias: "pistons",
        team: &NBA_DET,
    },
    AliasMapping {
        alias: "detroitpistons",
        team: &NBA_DET,
    },
    AliasMapping {
        alias: "gsw",
        team: &NBA_GSW,
    },
    AliasMapping {
        alias: "gs",
        team: &NBA_GSW,
    },
    AliasMapping {
        alias: "goldenstate",
        team: &NBA_GSW,
    },
    AliasMapping {
        alias: "warriors",
        team: &NBA_GSW,
    },
    AliasMapping {
        alias: "goldenstatewarriors",
        team: &NBA_GSW,
    },
    AliasMapping {
        alias: "gswarriors",
        team: &NBA_GSW,
    },
    AliasMapping {
        alias: "hou",
        team: &NBA_HOU,
    },
    AliasMapping {
        alias: "houston",
        team: &NBA_HOU,
    },
    AliasMapping {
        alias: "rockets",
        team: &NBA_HOU,
    },
    AliasMapping {
        alias: "houstonrockets",
        team: &NBA_HOU,
    },
    AliasMapping {
        alias: "ind",
        team: &NBA_IND,
    },
    AliasMapping {
        alias: "indiana",
        team: &NBA_IND,
    },
    AliasMapping {
        alias: "pacers",
        team: &NBA_IND,
    },
    AliasMapping {
        alias: "indianapacers",
        team: &NBA_IND,
    },
    AliasMapping {
        alias: "lac",
        team: &NBA_LAC,
    },
    AliasMapping {
        alias: "clippers",
        team: &NBA_LAC,
    },
    AliasMapping {
        alias: "laclippers",
        team: &NBA_LAC,
    },
    AliasMapping {
        alias: "losangelesclippers",
        team: &NBA_LAC,
    },
    AliasMapping {
        alias: "lal",
        team: &NBA_LAL,
    },
    AliasMapping {
        alias: "lakers",
        team: &NBA_LAL,
    },
    AliasMapping {
        alias: "lalakers",
        team: &NBA_LAL,
    },
    AliasMapping {
        alias: "losangeleslakers",
        team: &NBA_LAL,
    },
    AliasMapping {
        alias: "mem",
        team: &NBA_MEM,
    },
    AliasMapping {
        alias: "memphis",
        team: &NBA_MEM,
    },
    AliasMapping {
        alias: "grizzlies",
        team: &NBA_MEM,
    },
    AliasMapping {
        alias: "memphisgrizzlies",
        team: &NBA_MEM,
    },
    AliasMapping {
        alias: "mia",
        team: &NBA_MIA,
    },
    AliasMapping {
        alias: "miami",
        team: &NBA_MIA,
    },
    AliasMapping {
        alias: "heat",
        team: &NBA_MIA,
    },
    AliasMapping {
        alias: "miamiheat",
        team: &NBA_MIA,
    },
    AliasMapping {
        alias: "mil",
        team: &NBA_MIL,
    },
    AliasMapping {
        alias: "milwaukee",
        team: &NBA_MIL,
    },
    AliasMapping {
        alias: "bucks",
        team: &NBA_MIL,
    },
    AliasMapping {
        alias: "milwaukeebucks",
        team: &NBA_MIL,
    },
    AliasMapping {
        alias: "min",
        team: &NBA_MIN,
    },
    AliasMapping {
        alias: "minnesota",
        team: &NBA_MIN,
    },
    AliasMapping {
        alias: "timberwolves",
        team: &NBA_MIN,
    },
    AliasMapping {
        alias: "wolves",
        team: &NBA_MIN,
    },
    AliasMapping {
        alias: "minnesotatimberwolves",
        team: &NBA_MIN,
    },
    AliasMapping {
        alias: "nop",
        team: &NBA_NOP,
    },
    AliasMapping {
        alias: "no",
        team: &NBA_NOP,
    },
    AliasMapping {
        alias: "neworleans",
        team: &NBA_NOP,
    },
    AliasMapping {
        alias: "pelicans",
        team: &NBA_NOP,
    },
    AliasMapping {
        alias: "neworleanspelicans",
        team: &NBA_NOP,
    },
    AliasMapping {
        alias: "nyk",
        team: &NBA_NYK,
    },
    AliasMapping {
        alias: "ny",
        team: &NBA_NYK,
    },
    AliasMapping {
        alias: "knicks",
        team: &NBA_NYK,
    },
    AliasMapping {
        alias: "newyorkknicks",
        team: &NBA_NYK,
    },
    AliasMapping {
        alias: "nyknicks",
        team: &NBA_NYK,
    },
    AliasMapping {
        alias: "okc",
        team: &NBA_OKC,
    },
    AliasMapping {
        alias: "oklahomacity",
        team: &NBA_OKC,
    },
    AliasMapping {
        alias: "thunder",
        team: &NBA_OKC,
    },
    AliasMapping {
        alias: "oklahomacitythunder",
        team: &NBA_OKC,
    },
    AliasMapping {
        alias: "okcthunder",
        team: &NBA_OKC,
    },
    AliasMapping {
        alias: "orl",
        team: &NBA_ORL,
    },
    AliasMapping {
        alias: "orlando",
        team: &NBA_ORL,
    },
    AliasMapping {
        alias: "magic",
        team: &NBA_ORL,
    },
    AliasMapping {
        alias: "orlandomagic",
        team: &NBA_ORL,
    },
    AliasMapping {
        alias: "phi",
        team: &NBA_PHI,
    },
    AliasMapping {
        alias: "philadelphia",
        team: &NBA_PHI,
    },
    AliasMapping {
        alias: "76ers",
        team: &NBA_PHI,
    },
    AliasMapping {
        alias: "sixers",
        team: &NBA_PHI,
    },
    AliasMapping {
        alias: "philadelphia76ers",
        team: &NBA_PHI,
    },
    AliasMapping {
        alias: "phx",
        team: &NBA_PHX,
    },
    AliasMapping {
        alias: "phoenix",
        team: &NBA_PHX,
    },
    AliasMapping {
        alias: "suns",
        team: &NBA_PHX,
    },
    AliasMapping {
        alias: "phoenixsuns",
        team: &NBA_PHX,
    },
    AliasMapping {
        alias: "por",
        team: &NBA_POR,
    },
    AliasMapping {
        alias: "portland",
        team: &NBA_POR,
    },
    AliasMapping {
        alias: "trailblazers",
        team: &NBA_POR,
    },
    AliasMapping {
        alias: "blazers",
        team: &NBA_POR,
    },
    AliasMapping {
        alias: "portlandtrailblazers",
        team: &NBA_POR,
    },
    AliasMapping {
        alias: "sac",
        team: &NBA_SAC,
    },
    AliasMapping {
        alias: "sacramento",
        team: &NBA_SAC,
    },
    AliasMapping {
        alias: "kings",
        team: &NBA_SAC,
    },
    AliasMapping {
        alias: "sacramentokings",
        team: &NBA_SAC,
    },
    AliasMapping {
        alias: "sas",
        team: &NBA_SAS,
    },
    AliasMapping {
        alias: "sa",
        team: &NBA_SAS,
    },
    AliasMapping {
        alias: "sanantonio",
        team: &NBA_SAS,
    },
    AliasMapping {
        alias: "spurs",
        team: &NBA_SAS,
    },
    AliasMapping {
        alias: "sanantoniospurs",
        team: &NBA_SAS,
    },
    AliasMapping {
        alias: "tor",
        team: &NBA_TOR,
    },
    AliasMapping {
        alias: "toronto",
        team: &NBA_TOR,
    },
    AliasMapping {
        alias: "raptors",
        team: &NBA_TOR,
    },
    AliasMapping {
        alias: "torontoraptors",
        team: &NBA_TOR,
    },
    AliasMapping {
        alias: "uta",
        team: &NBA_UTA,
    },
    AliasMapping {
        alias: "utah",
        team: &NBA_UTA,
    },
    AliasMapping {
        alias: "jazz",
        team: &NBA_UTA,
    },
    AliasMapping {
        alias: "utahjazz",
        team: &NBA_UTA,
    },
    AliasMapping {
        alias: "was",
        team: &NBA_WAS,
    },
    AliasMapping {
        alias: "washington",
        team: &NBA_WAS,
    },
    AliasMapping {
        alias: "wizards",
        team: &NBA_WAS,
    },
    AliasMapping {
        alias: "washingtonwizards",
        team: &NBA_WAS,
    },
    // NFL Aliases
    AliasMapping {
        alias: "kc",
        team: &NFL_KC,
    },
    AliasMapping {
        alias: "kansascity",
        team: &NFL_KC,
    },
    AliasMapping {
        alias: "chiefs",
        team: &NFL_KC,
    },
    AliasMapping {
        alias: "kansascitychiefs",
        team: &NFL_KC,
    },
    AliasMapping {
        alias: "sf",
        team: &NFL_SF,
    },
    AliasMapping {
        alias: "sanfrancisco",
        team: &NFL_SF,
    },
    AliasMapping {
        alias: "49ers",
        team: &NFL_SF,
    },
    AliasMapping {
        alias: "niners",
        team: &NFL_SF,
    },
    AliasMapping {
        alias: "sanfrancisco49ers",
        team: &NFL_SF,
    },
    AliasMapping {
        alias: "bal",
        team: &NFL_BAL,
    },
    AliasMapping {
        alias: "baltimore",
        team: &NFL_BAL,
    },
    AliasMapping {
        alias: "ravens",
        team: &NFL_BAL,
    },
    AliasMapping {
        alias: "baltimoreravens",
        team: &NFL_BAL,
    },
    AliasMapping {
        alias: "buf",
        team: &NFL_BUF,
    },
    AliasMapping {
        alias: "buffalo",
        team: &NFL_BUF,
    },
    AliasMapping {
        alias: "bills",
        team: &NFL_BUF,
    },
    AliasMapping {
        alias: "buffalobills",
        team: &NFL_BUF,
    },
    AliasMapping {
        alias: "cowboys",
        team: &NFL_DAL,
    },
    AliasMapping {
        alias: "dallascowboys",
        team: &NFL_DAL,
    },
    AliasMapping {
        alias: "gb",
        team: &NFL_GB,
    },
    AliasMapping {
        alias: "greenbay",
        team: &NFL_GB,
    },
    AliasMapping {
        alias: "packers",
        team: &NFL_GB,
    },
    AliasMapping {
        alias: "greenbaypackers",
        team: &NFL_GB,
    },
    AliasMapping {
        alias: "ne",
        team: &NFL_NE,
    },
    AliasMapping {
        alias: "newengland",
        team: &NFL_NE,
    },
    AliasMapping {
        alias: "patriots",
        team: &NFL_NE,
    },
    AliasMapping {
        alias: "pats",
        team: &NFL_NE,
    },
    AliasMapping {
        alias: "newenglandpatriots",
        team: &NFL_NE,
    },
    AliasMapping {
        alias: "eagles",
        team: &NFL_PHI,
    },
    AliasMapping {
        alias: "philadelphiaeagles",
        team: &NFL_PHI,
    },
    // MLB Aliases — code, full name, and mascot for every club. Bare city
    // names are deliberately absent: New York, Chicago, and Los Angeles each
    // host two clubs, so a city-only label must stay unresolvable rather
    // than silently pick a franchise.
    AliasMapping {
        alias: "az",
        team: &MLB_AZ,
    },
    AliasMapping {
        alias: "arizonadiamondbacks",
        team: &MLB_AZ,
    },
    AliasMapping {
        alias: "diamondbacks",
        team: &MLB_AZ,
    },
    AliasMapping {
        alias: "dbacks",
        team: &MLB_AZ,
    },
    AliasMapping {
        alias: "atl",
        team: &MLB_ATL,
    },
    AliasMapping {
        alias: "atlantabraves",
        team: &MLB_ATL,
    },
    AliasMapping {
        alias: "braves",
        team: &MLB_ATL,
    },
    AliasMapping {
        alias: "ath",
        team: &MLB_ATH,
    },
    AliasMapping {
        alias: "athletics",
        team: &MLB_ATH,
    },
    AliasMapping {
        alias: "bal",
        team: &MLB_BAL,
    },
    AliasMapping {
        alias: "baltimoreorioles",
        team: &MLB_BAL,
    },
    AliasMapping {
        alias: "orioles",
        team: &MLB_BAL,
    },
    AliasMapping {
        alias: "bos",
        team: &MLB_BOS,
    },
    AliasMapping {
        alias: "boston",
        team: &MLB_BOS,
    },
    AliasMapping {
        alias: "redsox",
        team: &MLB_BOS,
    },
    AliasMapping {
        alias: "bostonredsox",
        team: &MLB_BOS,
    },
    AliasMapping {
        alias: "chc",
        team: &MLB_CHC,
    },
    AliasMapping {
        alias: "chicagocubs",
        team: &MLB_CHC,
    },
    AliasMapping {
        alias: "cubs",
        team: &MLB_CHC,
    },
    AliasMapping {
        alias: "cws",
        team: &MLB_CWS,
    },
    AliasMapping {
        alias: "chicagowhitesox",
        team: &MLB_CWS,
    },
    AliasMapping {
        alias: "whitesox",
        team: &MLB_CWS,
    },
    AliasMapping {
        alias: "cin",
        team: &MLB_CIN,
    },
    AliasMapping {
        alias: "cincinnatireds",
        team: &MLB_CIN,
    },
    AliasMapping {
        alias: "reds",
        team: &MLB_CIN,
    },
    AliasMapping {
        alias: "cle",
        team: &MLB_CLE,
    },
    AliasMapping {
        alias: "clevelandguardians",
        team: &MLB_CLE,
    },
    AliasMapping {
        alias: "guardians",
        team: &MLB_CLE,
    },
    AliasMapping {
        alias: "col",
        team: &MLB_COL,
    },
    AliasMapping {
        alias: "coloradorockies",
        team: &MLB_COL,
    },
    AliasMapping {
        alias: "rockies",
        team: &MLB_COL,
    },
    AliasMapping {
        alias: "det",
        team: &MLB_DET,
    },
    AliasMapping {
        alias: "detroittigers",
        team: &MLB_DET,
    },
    AliasMapping {
        alias: "tigers",
        team: &MLB_DET,
    },
    AliasMapping {
        alias: "hou",
        team: &MLB_HOU,
    },
    AliasMapping {
        alias: "houstonastros",
        team: &MLB_HOU,
    },
    AliasMapping {
        alias: "astros",
        team: &MLB_HOU,
    },
    AliasMapping {
        alias: "kc",
        team: &MLB_KC,
    },
    AliasMapping {
        alias: "kansascityroyals",
        team: &MLB_KC,
    },
    AliasMapping {
        alias: "royals",
        team: &MLB_KC,
    },
    AliasMapping {
        alias: "laa",
        team: &MLB_LAA,
    },
    AliasMapping {
        alias: "losangelesangels",
        team: &MLB_LAA,
    },
    AliasMapping {
        alias: "angels",
        team: &MLB_LAA,
    },
    AliasMapping {
        alias: "lad",
        team: &MLB_LAD,
    },
    AliasMapping {
        alias: "losangelesdodgers",
        team: &MLB_LAD,
    },
    AliasMapping {
        alias: "dodgers",
        team: &MLB_LAD,
    },
    AliasMapping {
        alias: "ladodgers",
        team: &MLB_LAD,
    },
    AliasMapping {
        alias: "mia",
        team: &MLB_MIA,
    },
    AliasMapping {
        alias: "miamimarlins",
        team: &MLB_MIA,
    },
    AliasMapping {
        alias: "marlins",
        team: &MLB_MIA,
    },
    AliasMapping {
        alias: "mil",
        team: &MLB_MIL,
    },
    AliasMapping {
        alias: "milwaukeebrewers",
        team: &MLB_MIL,
    },
    AliasMapping {
        alias: "brewers",
        team: &MLB_MIL,
    },
    AliasMapping {
        alias: "min",
        team: &MLB_MIN,
    },
    AliasMapping {
        alias: "minnesotatwins",
        team: &MLB_MIN,
    },
    AliasMapping {
        alias: "twins",
        team: &MLB_MIN,
    },
    AliasMapping {
        alias: "nym",
        team: &MLB_NYM,
    },
    AliasMapping {
        alias: "newyorkmets",
        team: &MLB_NYM,
    },
    AliasMapping {
        alias: "mets",
        team: &MLB_NYM,
    },
    AliasMapping {
        alias: "nyy",
        team: &MLB_NYY,
    },
    AliasMapping {
        alias: "yankees",
        team: &MLB_NYY,
    },
    AliasMapping {
        alias: "newyorkyankees",
        team: &MLB_NYY,
    },
    AliasMapping {
        alias: "phi",
        team: &MLB_PHI,
    },
    AliasMapping {
        alias: "philadelphiaphillies",
        team: &MLB_PHI,
    },
    AliasMapping {
        alias: "phillies",
        team: &MLB_PHI,
    },
    AliasMapping {
        alias: "pit",
        team: &MLB_PIT,
    },
    AliasMapping {
        alias: "pittsburghpirates",
        team: &MLB_PIT,
    },
    AliasMapping {
        alias: "pirates",
        team: &MLB_PIT,
    },
    AliasMapping {
        alias: "sd",
        team: &MLB_SD,
    },
    AliasMapping {
        alias: "sandiegopadres",
        team: &MLB_SD,
    },
    AliasMapping {
        alias: "padres",
        team: &MLB_SD,
    },
    AliasMapping {
        alias: "sea",
        team: &MLB_SEA,
    },
    AliasMapping {
        alias: "seattlemariners",
        team: &MLB_SEA,
    },
    AliasMapping {
        alias: "mariners",
        team: &MLB_SEA,
    },
    AliasMapping {
        alias: "sf",
        team: &MLB_SF,
    },
    AliasMapping {
        alias: "sanfranciscogiants",
        team: &MLB_SF,
    },
    AliasMapping {
        alias: "giants",
        team: &MLB_SF,
    },
    AliasMapping {
        alias: "stl",
        team: &MLB_STL,
    },
    AliasMapping {
        alias: "stlouiscardinals",
        team: &MLB_STL,
    },
    AliasMapping {
        alias: "cardinals",
        team: &MLB_STL,
    },
    AliasMapping {
        alias: "tb",
        team: &MLB_TB,
    },
    AliasMapping {
        alias: "tampabayrays",
        team: &MLB_TB,
    },
    AliasMapping {
        alias: "rays",
        team: &MLB_TB,
    },
    AliasMapping {
        alias: "tex",
        team: &MLB_TEX,
    },
    AliasMapping {
        alias: "texasrangers",
        team: &MLB_TEX,
    },
    AliasMapping {
        alias: "rangers",
        team: &MLB_TEX,
    },
    AliasMapping {
        alias: "tor",
        team: &MLB_TOR,
    },
    AliasMapping {
        alias: "torontobluejays",
        team: &MLB_TOR,
    },
    AliasMapping {
        alias: "bluejays",
        team: &MLB_TOR,
    },
    AliasMapping {
        alias: "wsh",
        team: &MLB_WSH,
    },
    AliasMapping {
        alias: "washingtonnationals",
        team: &MLB_WSH,
    },
    AliasMapping {
        alias: "nationals",
        team: &MLB_WSH,
    },
    // NHL Aliases
    AliasMapping {
        alias: "bos",
        team: &NHL_BOS,
    },
    AliasMapping {
        alias: "boston",
        team: &NHL_BOS,
    },
    AliasMapping {
        alias: "bruins",
        team: &NHL_BOS,
    },
    AliasMapping {
        alias: "bostonbruins",
        team: &NHL_BOS,
    },
    AliasMapping {
        alias: "mapleleafs",
        team: &NHL_TOR,
    },
    AliasMapping {
        alias: "leafs",
        team: &NHL_TOR,
    },
    AliasMapping {
        alias: "torontomapleleafs",
        team: &NHL_TOR,
    },
    AliasMapping {
        alias: "edm",
        team: &NHL_EDM,
    },
    AliasMapping {
        alias: "edmonton",
        team: &NHL_EDM,
    },
    AliasMapping {
        alias: "oilers",
        team: &NHL_EDM,
    },
    AliasMapping {
        alias: "edmontonoilers",
        team: &NHL_EDM,
    },
];

/// Normalize a raw string for alias dictionary lookup.
///
/// Strips punctuation, parentheses, brackets, and whitespace, returning a lowercase alphanumeric string.
pub fn normalize_string(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Lookup a canonical team from a raw name, abbreviation, or alias.
///
/// If `sport_hint` is provided, candidates matching the specified sport will be preferred.
pub fn lookup_team(raw: &str, sport_hint: Option<Sport>) -> Result<&'static CanonicalTeam> {
    let normalized = normalize_string(raw);
    if normalized.is_empty() {
        return Err(MatchError::UnrecognizedTeam(raw.to_string()));
    }

    let mut best_match: Option<&'static CanonicalTeam> = None;

    for mapping in ALIASES {
        if mapping.alias == normalized {
            if let Some(sport) = sport_hint {
                if mapping.team.sport == sport {
                    return Ok(mapping.team);
                }
            }
            if best_match.is_none() {
                best_match = Some(mapping.team);
            }
        }
    }

    if let Some(team) = best_match {
        return Ok(team);
    }

    // Secondary pass: prefix or suffix exact match if applicable
    for mapping in ALIASES {
        if mapping.team.code.eq_ignore_ascii_case(&normalized) {
            if let Some(sport) = sport_hint {
                if mapping.team.sport == sport {
                    return Ok(mapping.team);
                }
            }
            if best_match.is_none() {
                best_match = Some(mapping.team);
            }
        }
    }

    best_match.ok_or_else(|| MatchError::UnrecognizedTeam(raw.to_string()))
}

/// Lookup a canonical team from a raw name, abbreviation, or alias, rejecting
/// aliases that match more than one canonical team across all sports.
///
/// Unlike [`lookup_team`], this never guesses when a sport hint is absent:
/// a city-only alias such as `"Boston"` (Celtics, Red Sox, and Bruins) is an
/// error, while a mascot or full-name alias that belongs to exactly one team
/// still resolves. Discovery code pairing instruments across venues uses this
/// so an ambiguous venue label can never silently attach to the wrong sport.
pub fn lookup_team_unique(raw: &str) -> Result<&'static CanonicalTeam> {
    let normalized = normalize_string(raw);
    if normalized.is_empty() {
        return Err(MatchError::UnrecognizedTeam(raw.to_string()));
    }

    let mut primary: Option<&'static CanonicalTeam> = None;
    for mapping in ALIASES {
        if mapping.alias == normalized {
            if let Some(first) = primary {
                if first != mapping.team {
                    return Err(MatchError::AmbiguousTeam(raw.to_string()));
                }
            } else {
                primary = Some(mapping.team);
            }
        }
    }

    if let Some(team) = primary {
        return Ok(team);
    }

    // Secondary pass: exact team-code match only, with the same uniqueness rule.
    let mut secondary: Option<&'static CanonicalTeam> = None;
    for mapping in ALIASES {
        if mapping.team.code.eq_ignore_ascii_case(&normalized) {
            if let Some(first) = secondary {
                if first != mapping.team {
                    return Err(MatchError::AmbiguousTeam(raw.to_string()));
                }
            } else {
                secondary = Some(mapping.team);
            }
        }
    }

    secondary.ok_or_else(|| MatchError::UnrecognizedTeam(raw.to_string()))
}

/// Parse a raw matchup string into canonical home and away teams.
///
/// Supports common patterns:
/// - `"BOS @ LAL"` or `"Boston Celtics at Los Angeles Lakers"` (First is away, second is home)
/// - `"Los Angeles Lakers vs Boston Celtics"` or `"LAL vs. BOS"` or `"LAL v BOS"` (First is home, second is away)
pub fn parse_matchup(raw: &str, sport_hint: Option<Sport>) -> Result<Matchup> {
    let trimmed = raw.trim();

    // Check for "@" or " at " indicating Away @ Home
    if let Some((away_part, home_part)) = split_matchup(trimmed, &[" @ ", "@", " at "]) {
        let away = lookup_team(away_part, sport_hint)?;
        let home = lookup_team(home_part, sport_hint)?;
        return Ok(Matchup::new(home, away));
    }

    // Check for "vs", "vs.", "v", "v." indicating Home vs Away
    if let Some((home_part, away_part)) =
        split_matchup(trimmed, &[" vs. ", " vs ", " v. ", " v ", " - "])
    {
        let home = lookup_team(home_part, sport_hint)?;
        let away = lookup_team(away_part, sport_hint)?;
        return Ok(Matchup::new(home, away));
    }

    Err(MatchError::MalformedMatchup(raw.to_string()))
}

fn split_matchup<'a>(input: &'a str, delimiters: &[&str]) -> Option<(&'a str, &'a str)> {
    for &delim in delimiters {
        if let Some(idx) = input.find(delim) {
            let left = input[..idx].trim();
            let right = input[idx + delim.len()..].trim();
            if !left.is_empty() && !right.is_empty() {
                return Some((left, right));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_name_normalization_resolves_common_aliases() {
        let bos = lookup_team("Boston Celtics", Some(Sport::Nba)).unwrap();
        assert_eq!(bos.code, "BOS");

        let bos2 = lookup_team("BOS", Some(Sport::Nba)).unwrap();
        assert_eq!(bos2.code, "BOS");

        let bos3 = lookup_team("Celtics", Some(Sport::Nba)).unwrap();
        assert_eq!(bos3.code, "BOS");

        let lal = lookup_team("LA Lakers", Some(Sport::Nba)).unwrap();
        assert_eq!(lal.code, "LAL");

        let gsw = lookup_team("GS Warriors", Some(Sport::Nba)).unwrap();
        assert_eq!(gsw.code, "GSW");

        let phi = lookup_team("76ers", Some(Sport::Nba)).unwrap();
        assert_eq!(phi.code, "PHI");
    }

    #[test]
    fn sport_hints_disambiguate_shared_cities() {
        let nba_bos = lookup_team("Boston", Some(Sport::Nba)).unwrap();
        assert_eq!(nba_bos.sport, Sport::Nba);
        assert_eq!(nba_bos.code, "BOS");
        assert_eq!(nba_bos.mascot, "Celtics");

        let mlb_bos = lookup_team("Boston", Some(Sport::Mlb)).unwrap();
        assert_eq!(mlb_bos.sport, Sport::Mlb);
        assert_eq!(mlb_bos.code, "BOS");
        assert_eq!(mlb_bos.mascot, "Red Sox");

        let nhl_bos = lookup_team("Boston", Some(Sport::Nhl)).unwrap();
        assert_eq!(nhl_bos.sport, Sport::Nhl);
        assert_eq!(nhl_bos.code, "BOS");
        assert_eq!(nhl_bos.mascot, "Bruins");
    }

    #[test]
    fn matchup_parsing_at_symbol() {
        let matchup = parse_matchup("BOS @ LAL", Some(Sport::Nba)).unwrap();
        assert_eq!(matchup.away.code, "BOS");
        assert_eq!(matchup.home.code, "LAL");

        let matchup2 =
            parse_matchup("Boston Celtics at Los Angeles Lakers", Some(Sport::Nba)).unwrap();
        assert_eq!(matchup2.away.code, "BOS");
        assert_eq!(matchup2.home.code, "LAL");
    }

    #[test]
    fn matchup_parsing_vs_symbol() {
        let matchup = parse_matchup("LAL vs. BOS", Some(Sport::Nba)).unwrap();
        assert_eq!(matchup.home.code, "LAL");
        assert_eq!(matchup.away.code, "BOS");

        let matchup2 =
            parse_matchup("Los Angeles Lakers vs Boston Celtics", Some(Sport::Nba)).unwrap();
        assert_eq!(matchup2.home.code, "LAL");
        assert_eq!(matchup2.away.code, "BOS");
    }

    #[test]
    fn unknown_teams_or_malformed_matchups_return_errors() {
        assert!(matches!(
            lookup_team("Atlantis Atlantians", None),
            Err(MatchError::UnrecognizedTeam(_))
        ));

        assert!(matches!(
            parse_matchup("JustATeamWithoutSeparator", None),
            Err(MatchError::MalformedMatchup(_))
        ));
    }

    #[test]
    fn mlb_roster_resolves_every_live_kalshi_code_and_poly_label() {
        // Codes exactly as embedded in live KXMLB tickers (public markets
        // API, Aug 2026), including the two-letter codes a fixed 3+3 split
        // can never recover.
        const CODES: [&str; 30] = [
            "AZ", "ATL", "ATH", "BAL", "BOS", "CHC", "CWS", "CIN", "CLE", "COL", "DET", "HOU",
            "KC", "LAA", "LAD", "MIA", "MIL", "MIN", "NYM", "NYY", "PHI", "PIT", "SD", "SEA", "SF",
            "STL", "TB", "TEX", "TOR", "WSH",
        ];
        for code in CODES {
            let team = lookup_team(code, Some(Sport::Mlb))
                .unwrap_or_else(|e| panic!("code {code} must resolve: {e}"));
            assert_eq!(team.code, code);
            assert_eq!(team.sport, Sport::Mlb);

            // Polymarket outcome labels are these exact full names, resolved
            // without a sport hint on the strict uniqueness path.
            let labeled = lookup_team_unique(team.full_name)
                .unwrap_or_else(|e| panic!("label {:?} must be unique: {e}", team.full_name));
            assert_eq!(labeled.code, code);
        }
    }

    #[test]
    fn mlb_shared_city_labels_stay_unresolvable() {
        // Two clubs share each of these cities inside MLB alone. No alias may
        // silently bind a bare city name to one franchise; if the hinted
        // lookup falls back cross-sport, that is an error upstream code
        // rejects via the strict uniqueness path, never an MLB guess.
        for city in ["New York", "Chicago", "Los Angeles"] {
            if let Ok(team) = lookup_team(city, Some(Sport::Mlb)) {
                assert_ne!(
                    team.sport,
                    Sport::Mlb,
                    "{city} must not silently resolve to one MLB club"
                );
            }
        }
        // Mascots owned by exactly one franchise resolve hint-free.
        assert_eq!(lookup_team_unique("Guardians").unwrap().code, "CLE");
        assert_eq!(lookup_team_unique("Blue Jays").unwrap().code, "TOR");
        assert_eq!(lookup_team_unique("Cubs").unwrap().code, "CHC");
        assert_eq!(lookup_team_unique("White Sox").unwrap().code, "CWS");
        // City labels shared across leagues stay ambiguous on the strict path.
        assert!(matches!(
            lookup_team_unique("Boston"),
            Err(MatchError::AmbiguousTeam(_))
        ));
    }
}
