//! Tournament formats and their game schedules.
//!
//! The schedule is pure data: it says which two engines play in each game and
//! whether an opening position is used. The app turns that into board games.
//!
//! Three formats are supported:
//!
//! * **Match** — two engines play `games` games and the colours alternate.
//! * **Round-Robin** — three engines; every pairing plays
//!   `games_per_encounter` games (one with each colour when the value is 2,
//!   the default) and the whole set repeats `rounds` times.
//! * **Gauntlet** — three engines; the first engine plays every other engine,
//!   with the same per-encounter colour balance and round count.
//!
//! Multi-engine formats always play each encounter in both colours; the
//! `reverse_colors` switch only affects a two-engine match, where it is the
//! only thing that decides the colour of the next game.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{BookOrder, TournamentConfig};

/// Games per pairing in a Round-Robin or Gauntlet round.
pub const DEFAULT_GAMES_PER_ENCOUNTER: u32 = 2;
/// Number of times the complete pairing schedule is played.
pub const DEFAULT_ROUNDS: u32 = 1;

/// The tournament format chosen in the dialog.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TournamentFormat {
    /// Two engines, a fixed number of games, alternating colours.
    #[default]
    Match,
    /// Every engine plays every other engine.
    RoundRobin,
    /// The first engine plays each of the others.
    Gauntlet,
}

impl TournamentFormat {
    /// Every format, in menu order.
    pub const ALL: [Self; 3] = [Self::Match, Self::RoundRobin, Self::Gauntlet];

    /// The name shown in the dialog and the PGN event default.
    pub fn label(self) -> &'static str {
        match self {
            Self::Match => "Match",
            Self::RoundRobin => "Round-Robin",
            Self::Gauntlet => "Gauntlet",
        }
    }

    /// How many engines the format needs.
    pub fn engine_count(self) -> usize {
        match self {
            Self::Match => 2,
            Self::RoundRobin | Self::Gauntlet => 3,
        }
    }

    /// Whether the dialog uses "games per encounter" and "rounds" instead of
    /// one total game count.
    pub fn uses_rounds(self) -> bool {
        self != Self::Match
    }
}

/// One game in the schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledGame {
    /// 1-based game number, also the PGN `Round` tag.
    pub number: u32,
    /// Index into [`TournamentConfig::engines`] of the White player.
    pub white: usize,
    /// Index into [`TournamentConfig::engines`] of the Black player.
    pub black: usize,
    /// EPD position index, when a book is in use.
    pub opening: Option<usize>,
}

/// The pairings of one complete multi-engine round, as engine indices.
///
/// A match has one implicit encounter and does not use this list.
pub fn encounters(format: TournamentFormat) -> &'static [(usize, usize)] {
    match format {
        TournamentFormat::Match => &[(0, 1)],
        TournamentFormat::RoundRobin => &[(0, 1), (0, 2), (1, 2)],
        // The first engine is the gauntlet player; the other two are its
        // opponents.
        TournamentFormat::Gauntlet => &[(0, 1), (0, 2)],
    }
}

/// How many games the configuration will play.
pub fn total_games(config: &TournamentConfig) -> u32 {
    if config.format == TournamentFormat::Match {
        return config.games;
    }
    let per_round =
        encounters(config.format).len() as u64 * u64::from(config.games_per_encounter.max(1));
    per_round
        .saturating_mul(u64::from(config.rounds))
        .min(u64::from(u32::MAX)) as u32
}

/// Build the schedule with a fresh random seed for the opening book.
pub fn schedule_games(config: &TournamentConfig, opening_count: usize) -> Vec<ScheduledGame> {
    schedule_games_with_seed(config, opening_count, random_seed())
}

/// [`schedule_games`] with a fixed shuffle seed, for tests.
pub fn schedule_games_with_seed(
    config: &TournamentConfig,
    opening_count: usize,
    seed: u64,
) -> Vec<ScheduledGame> {
    let mut rng = XorShift::with_seed(seed);
    let mut permutation: Vec<usize> = (0..opening_count).collect();
    let mut shuffled = false;
    let total = total_games(config);
    let mut games = Vec::with_capacity(total as usize);
    for index in 0..total {
        let (white, black) = players_for_game(config, index);
        let opening = if opening_count == 0 {
            None
        } else {
            let position = index as usize % opening_count;
            let random = config
                .opening_book
                .as_ref()
                .is_some_and(|book| book.order == BookOrder::Random);
            if random {
                if !shuffled || position == 0 {
                    shuffle(&mut permutation, &mut rng);
                    shuffled = true;
                }
                Some(permutation[position])
            } else {
                Some(position)
            }
        };
        games.push(ScheduledGame {
            number: index + 1,
            white,
            black,
            opening,
        });
    }
    games
}

/// The engine indices that play White and Black in game `index` (zero-based).
fn players_for_game(config: &TournamentConfig, index: u32) -> (usize, usize) {
    if config.format == TournamentFormat::Match {
        let first_is_white = !(config.reverse_colors && index % 2 == 1);
        return if first_is_white { (0, 1) } else { (1, 0) };
    }

    let pairings = encounters(config.format);
    let games_per_encounter = config.games_per_encounter.max(1);
    let per_round = pairings.len() as u32 * games_per_encounter;
    let within_round = index % per_round;
    let pair = pairings[(within_round / games_per_encounter) as usize];
    let leg = within_round % games_per_encounter;
    if leg.is_multiple_of(2) {
        pair
    } else {
        (pair.1, pair.0)
    }
}

/// Fisher-Yates shuffle.
fn shuffle(values: &mut [usize], rng: &mut XorShift) {
    for i in (1..values.len()).rev() {
        let j = (rng.next_u64() % (i as u64 + 1)) as usize;
        values.swap(i, j);
    }
}

/// Small xorshift generator, matching the opening book picker's approach.
struct XorShift {
    state: u64,
}

impl XorShift {
    fn with_seed(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }
}

/// A seed from the clock, so a restarted run gets a different book order.
fn random_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0x9E37_79B9_7F4A_7C15, |duration| duration.as_nanos() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tournament::EngineSpec;

    fn config(format: TournamentFormat) -> TournamentConfig {
        TournamentConfig {
            format,
            white: EngineSpec::from_path("/tmp/one".into()),
            black: EngineSpec::from_path("/tmp/two".into()),
            third: EngineSpec::from_path("/tmp/three".into()),
            games_per_encounter: 2,
            rounds: 1,
            ..TournamentConfig::default()
        }
    }

    #[test]
    fn match_colours_alternate_when_reverse_colors_is_on() {
        let config = TournamentConfig {
            games: 4,
            reverse_colors: true,
            ..config(TournamentFormat::Match)
        };
        let games = schedule_games_with_seed(&config, 0, 1);
        let whites: Vec<usize> = games.iter().map(|game| game.white).collect();
        assert_eq!(whites, vec![0, 1, 0, 1]);

        let fixed = TournamentConfig {
            reverse_colors: false,
            ..config
        };
        let games = schedule_games_with_seed(&fixed, 0, 1);
        assert!(games.iter().all(|game| game.white == 0));
    }

    #[test]
    fn round_robin_plays_every_pairing_in_both_colours() {
        let config = config(TournamentFormat::RoundRobin);
        let games = schedule_games_with_seed(&config, 0, 1);
        assert_eq!(total_games(&config), 6);
        assert_eq!(games.len(), 6);
        for pair in [(0, 1), (0, 2), (1, 2)] {
            assert!(
                games
                    .iter()
                    .any(|game| game.white == pair.0 && game.black == pair.1),
                "missing {} as White against {}",
                pair.0,
                pair.1
            );
            assert!(
                games
                    .iter()
                    .any(|game| game.white == pair.1 && game.black == pair.0),
                "missing the reverse colour game for {} and {}",
                pair.0,
                pair.1
            );
        }
    }

    #[test]
    fn rounds_multiply_the_multi_engine_schedule() {
        let config = TournamentConfig {
            rounds: 3,
            ..config(TournamentFormat::RoundRobin)
        };
        assert_eq!(total_games(&config), 18);
        assert_eq!(schedule_games_with_seed(&config, 0, 1).len(), 18);
    }

    #[test]
    fn gauntlet_plays_the_first_engine_against_each_opponent() {
        let config = config(TournamentFormat::Gauntlet);
        let games = schedule_games_with_seed(&config, 0, 1);
        assert_eq!(total_games(&config), 4);
        assert!(games.iter().all(|game| game.white == 0 || game.black == 0));
        for opponent in [1, 2] {
            assert!(games
                .iter()
                .any(|game| game.white == 0 && game.black == opponent));
            assert!(games
                .iter()
                .any(|game| game.white == opponent && game.black == 0));
        }
    }

    #[test]
    fn sequential_book_order_walks_the_file_and_wraps() {
        let mut config = config(TournamentFormat::Match);
        config.games = 5;
        config.opening_book = Some(super::super::OpeningBookConfig {
            path: "book.epd".into(),
            order: BookOrder::Sequential,
        });
        let games = schedule_games_with_seed(&config, 3, 7);
        let openings: Vec<Option<usize>> = games.iter().map(|game| game.opening).collect();
        assert_eq!(openings, vec![Some(0), Some(1), Some(2), Some(0), Some(1)]);
    }

    #[test]
    fn random_book_order_uses_a_shuffled_permutation() {
        let mut config = config(TournamentFormat::Match);
        config.games = 6;
        config.opening_book = Some(super::super::OpeningBookConfig {
            path: "book.epd".into(),
            order: BookOrder::Random,
        });
        let games = schedule_games_with_seed(&config, 6, 42);
        let openings: Vec<usize> = games
            .iter()
            .map(|game| game.opening.expect("book position"))
            .collect();
        let mut sorted = openings.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2, 3, 4, 5], "a permutation");
        let again = schedule_games_with_seed(&config, 6, 42);
        assert_eq!(
            openings,
            again
                .iter()
                .map(|game| game.opening.expect("book position"))
                .collect::<Vec<_>>(),
            "the seed makes the order repeatable"
        );
    }
}
