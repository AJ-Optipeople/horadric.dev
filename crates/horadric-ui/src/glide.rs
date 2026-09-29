//! Windows gliding to a new place on screen instead of jumping there: a
//! cluster let go of after a drag, the ones below a cluster that grew, a
//! column scrolled by the wheel.
//!
//! Pure: the app says where each window is and where it should be, and
//! moves the windows to where [`Glides::step`] says they have got to.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::motion;

/// Half the way there every this long. A little slower than a tile's
/// slide, since a whole window is more to follow with the eye.
pub const HALF_LIFE: Duration = Duration::from_millis(55);

/// A window on its way, in physical pixels.
#[derive(Debug, Clone, Copy)]
struct Glide {
    x: f32,
    y: f32,
    to: (i32, i32),
    /// Where it was last put, to notice someone else moving it.
    put: (i32, i32),
}

/// Every window gliding, by an id the app picks (its handle).
#[derive(Default)]
pub struct Glides {
    on: HashMap<isize, Glide>,
    /// Windows placed before. A window's first place is where it starts,
    /// so it goes there at once.
    known: HashSet<isize>,
}

impl Glides {
    /// Sends the window at `at` toward `to`. Returns where to put it now
    /// when it should jump there instead: its first place, a window coming
    /// from off every screen, or with `animate` off. None when it is on
    /// its way, or already there.
    pub fn aim(
        &mut self,
        id: isize,
        at: (i32, i32),
        to: (i32, i32),
        animate: bool,
    ) -> Option<(i32, i32)> {
        let first = self.known.insert(id);
        if first || !animate || far_off(at) {
            self.on.remove(&id);
            return (at != to).then_some(to);
        }
        match self.on.get_mut(&id) {
            // Still where it was put: keep its speed, change its aim.
            Some(g) if g.put == at => g.to = to,
            _ if at == to => {
                self.on.remove(&id);
            }
            _ => {
                self.on.insert(
                    id,
                    Glide {
                        x: at.0 as f32,
                        y: at.1 as f32,
                        to,
                        put: at,
                    },
                );
            }
        }
        None
    }

    /// Moves every glide on by `dt`, and returns the windows to put and
    /// where. `at` says where a window really is: one someone else has
    /// moved since, a drag taking hold of it, is let go.
    pub fn step(
        &mut self,
        dt: Duration,
        at: impl Fn(isize) -> (i32, i32),
    ) -> Vec<(isize, (i32, i32))> {
        let mut moves = Vec::new();
        self.on.retain(|&id, g| {
            if at(id) != g.put {
                return false;
            }
            g.x = motion::approach(g.x, g.to.0 as f32, dt, HALF_LIFE);
            g.y = motion::approach(g.y, g.to.1 as f32, dt, HALF_LIFE);
            // Within half a pixel is there.
            if (g.x - g.to.0 as f32).abs() < 0.5 {
                g.x = g.to.0 as f32;
            }
            if (g.y - g.to.1 as f32).abs() < 0.5 {
                g.y = g.to.1 as f32;
            }
            let p = (g.x.round() as i32, g.y.round() as i32);
            if p != g.put {
                g.put = p;
                moves.push((id, p));
            }
            p != g.to
        });
        moves
    }

    /// Where the window is going, if it is on its way.
    pub fn target(&self, id: isize) -> Option<(i32, i32)> {
        self.on.get(&id).map(|g| g.to)
    }

    pub fn moving(&self) -> bool {
        !self.on.is_empty()
    }

    /// Stops the window where it is, since something else moves it now: a
    /// pane taken by the mouse. Its next aim glides from there.
    pub fn halt(&mut self, id: isize) {
        self.on.remove(&id);
    }

    /// The window is gone. Its handle may be given to another.
    pub fn forget(&mut self, id: isize) {
        self.on.remove(&id);
        self.known.remove(&id);
    }
}

/// Windows are born far off screen and moved into view once laid out.
fn far_off(at: (i32, i32)) -> bool {
    at.0 <= -5000 || at.1 <= -5000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_first_place_is_jumped_to() {
        let mut g = Glides::default();
        assert_eq!(g.aim(1, (0, 0), (10, 20), true), Some((10, 20)));
        assert!(!g.moving());
    }

    #[test]
    fn a_window_from_off_screen_jumps() {
        let mut g = Glides::default();
        g.aim(1, (0, 0), (0, 0), true);
        assert_eq!(g.aim(1, (-10_000, -10_000), (5, 5), true), Some((5, 5)));
    }

    #[test]
    fn without_animation_everything_jumps() {
        let mut g = Glides::default();
        g.aim(1, (0, 0), (0, 0), true);
        assert_eq!(g.aim(1, (0, 0), (0, 100), false), Some((0, 100)));
        assert_eq!(g.aim(1, (0, 100), (0, 100), false), None);
    }

    #[test]
    fn a_known_window_glides_and_lands() {
        let mut g = Glides::default();
        g.aim(1, (0, 0), (0, 0), true);
        assert_eq!(g.aim(1, (0, 0), (0, 100), true), None);
        assert_eq!(g.target(1), Some((0, 100)));
        let moves = g.step(HALF_LIFE, |_| (0, 0));
        assert_eq!(moves, vec![(1, (0, 50))]);
        let mut at = (0, 50);
        for _ in 0..40 {
            for (_, p) in g.step(ms(16), |_| at) {
                at = p;
            }
        }
        assert_eq!(at, (0, 100));
        assert!(!g.moving());
    }

    #[test]
    fn a_new_aim_mid_glide_keeps_going_from_where_it_is() {
        let mut g = Glides::default();
        g.aim(1, (0, 0), (0, 0), true);
        g.aim(1, (0, 0), (0, 100), true);
        g.step(HALF_LIFE, |_| (0, 0));
        g.aim(1, (0, 50), (0, 0), true);
        assert_eq!(g.step(HALF_LIFE, |_| (0, 50)), vec![(1, (0, 25))]);
    }

    #[test]
    fn a_window_moved_by_someone_else_is_let_go() {
        let mut g = Glides::default();
        g.aim(1, (0, 0), (0, 0), true);
        g.aim(1, (0, 0), (0, 100), true);
        assert!(g.step(ms(16), |_| (300, 300)).is_empty());
        assert!(!g.moving());
    }

    #[test]
    fn a_forgotten_window_starts_over() {
        let mut g = Glides::default();
        g.aim(1, (0, 0), (0, 0), true);
        g.forget(1);
        assert_eq!(g.aim(1, (0, 0), (0, 100), true), Some((0, 100)));
    }

    #[test]
    fn a_halted_window_stays_put_and_glides_from_there_next() {
        let mut g = Glides::default();
        g.aim(1, (0, 0), (0, 0), true);
        g.aim(1, (0, 0), (0, 100), true);
        g.halt(1);
        assert!(!g.moving());
        assert_eq!(g.aim(1, (40, 40), (0, 100), true), None);
        assert_eq!(g.target(1), Some((0, 100)));
    }
}
