//! The Town Portal key on the stage's caption: a scroll while the portal
//! is closed, a blue portal once it opens, swirling while a phone is
//! through it. What it says and how it moves are here, tested; the
//! drawing is `render.rs`.

use std::time::Duration;

/// What the key shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Look {
    /// Town Portal is off: the scroll, still rolled.
    #[default]
    Closed,
    /// On, but the relay is not reached yet: the portal, faint.
    Opening,
    /// On and through to the relay, no phone yet.
    Open,
    /// This many phones are through it.
    Phones(usize),
}

/// The look for a portal turned `on`, `connected` to the relay, with
/// `phones` through it.
pub fn look(on: bool, connected: bool, phones: usize) -> Look {
    match (on, connected, phones) {
        (false, _, _) => Look::Closed,
        (true, false, _) => Look::Opening,
        (true, true, 0) => Look::Open,
        (true, true, n) => Look::Phones(n),
    }
}

impl Look {
    /// The word beside the art.
    pub fn label(self) -> String {
        match self {
            Look::Closed => "Town Portal".into(),
            Look::Opening => "Opening".into(),
            Look::Open => "Portal open".into(),
            Look::Phones(1) => "1 phone".into(),
            Look::Phones(n) => format!("{n} phones"),
        }
    }

    /// What a click does, for the tip.
    pub fn tip(self) -> &'static str {
        match self {
            Look::Closed => "Open a Town Portal: your sessions on your phone",
            Look::Opening => "Reaching the relay. Click to pair a phone or close the portal",
            Look::Open => "Town Portal is open. Click to pair a phone or close it",
            Look::Phones(_) => "A phone is through the portal. Click to pair another or close it",
        }
    }

    /// Whether the portal stands at all, rather than the scroll.
    pub fn is_open(self) -> bool {
        self != Look::Closed
    }

    /// Whether it swirls, which takes a frame now and then. Only while a
    /// phone is through, so an idle stage draws nothing.
    pub fn swirls(self) -> bool {
        matches!(self, Look::Phones(_))
    }
}

/// How long the scroll takes to become the portal.
pub const OPENING: Duration = Duration::from_millis(700);

/// How far the scroll has become the portal, 0 to 1, `elapsed` after the
/// portal opened: fast at first and settling, as a spell that catches.
pub fn opened(elapsed: Duration) -> f32 {
    let t = (elapsed.as_secs_f32() / OPENING.as_secs_f32()).clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// How far the portal's light has turned, in radians, `elapsed` into its
/// swirl: one turn every four seconds, slow enough to read as calm.
pub fn spin(elapsed: Duration) -> f32 {
    (elapsed.as_secs_f32() / 4.0).fract() * std::f32::consts::TAU
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_look_follows_the_link() {
        assert_eq!(look(false, true, 2), Look::Closed);
        assert_eq!(look(true, false, 1), Look::Opening);
        assert_eq!(look(true, true, 0), Look::Open);
        assert_eq!(look(true, true, 2), Look::Phones(2));
    }

    #[test]
    fn the_label_counts_phones() {
        assert_eq!(Look::Closed.label(), "Town Portal");
        assert_eq!(Look::Phones(1).label(), "1 phone");
        assert_eq!(Look::Phones(3).label(), "3 phones");
    }

    #[test]
    fn only_a_phone_makes_it_swirl() {
        assert!(!Look::Closed.swirls());
        assert!(!Look::Open.swirls());
        assert!(Look::Phones(1).swirls());
        assert!(!Look::Closed.is_open() && Look::Opening.is_open());
    }

    #[test]
    fn the_scroll_opens_once_and_stays_open() {
        assert_eq!(opened(Duration::ZERO), 0.0);
        assert!(opened(OPENING / 4) > 0.25);
        assert_eq!(opened(OPENING), 1.0);
        assert_eq!(opened(OPENING * 5), 1.0);
    }

    #[test]
    fn the_swirl_turns_once_every_four_seconds() {
        assert_eq!(spin(Duration::ZERO), 0.0);
        assert!((spin(Duration::from_secs(1)) - std::f32::consts::FRAC_PI_2).abs() < 1e-4);
        assert!(spin(Duration::from_secs(4)) < 1e-4);
    }
}
