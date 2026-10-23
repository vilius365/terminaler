//! Time-based tweens for pane geometry and overlay alpha.
//!
//! Pure state: nothing here touches the renderer, so the progress maths is
//! unit tested. The window owns one `PaneAnimations`, starts tweens from
//! layout events, and asks for the interpolated value on every paint.
use config::EasingFunction;
use mux::pane::PaneId;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Pixel rectangle in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PxRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl PxRect {
    pub fn lerp(&self, to: &PxRect, t: f32) -> PxRect {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        PxRect {
            x: mix(self.x, to.x),
            y: mix(self.y, to.y),
            w: mix(self.w, to.w),
            h: mix(self.h, to.h),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Tween {
    from: PxRect,
    to: PxRect,
    start: Instant,
}

/// A pane that left the mux tree; its last rect fades out.
#[derive(Debug, Clone, Copy)]
pub struct Ghost {
    pub rect: PxRect,
    start: Instant,
}

/// What a fade-in belongs to. One entry per visible overlay state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FadeKey {
    DragSource(PaneId),
    DragTarget(PaneId),
    LongPress(PaneId),
    SplitHover(usize),
}

#[derive(Default)]
pub struct PaneAnimations {
    tweens: HashMap<PaneId, Tween>,
    ghosts: Vec<Ghost>,
    fades: HashMap<FadeKey, Instant>,
}

/// Eased progress in 0..=1 of an animation that began at `start`.
pub fn progress(start: Instant, now: Instant, duration: Duration) -> f32 {
    if duration.is_zero() {
        return 1.0;
    }
    let elapsed = now.saturating_duration_since(start).as_secs_f32();
    let linear = (elapsed / duration.as_secs_f32()).clamp(0.0, 1.0);
    EasingFunction::EaseOut
        .evaluate_at_position(linear)
        .clamp(0.0, 1.0)
}

impl PaneAnimations {
    /// Begin (or retarget) a tween. A retarget starts from the rect the
    /// pane currently shows, so rapid layout changes do not jump.
    pub fn start(
        &mut self,
        pane: PaneId,
        from: PxRect,
        to: PxRect,
        now: Instant,
        duration: Duration,
    ) {
        let from = self.current(pane, now, duration).unwrap_or(from);
        if from != to {
            self.tweens.insert(
                pane,
                Tween {
                    from,
                    to,
                    start: now,
                },
            );
        }
    }

    pub fn add_ghost(&mut self, rect: PxRect, now: Instant) {
        self.ghosts.push(Ghost { rect, start: now });
    }

    /// The rect to draw for `pane` now, or None when it is not animating.
    pub fn current(&self, pane: PaneId, now: Instant, duration: Duration) -> Option<PxRect> {
        let tween = self.tweens.get(&pane)?;
        let t = progress(tween.start, now, duration);
        Some(tween.from.lerp(&tween.to, t))
    }

    /// Ghost rects with their remaining opacity (1 -> 0).
    pub fn ghosts(&self, now: Instant, duration: Duration) -> Vec<(PxRect, f32)> {
        self.ghosts
            .iter()
            .map(|g| (g.rect, 1.0 - progress(g.start, now, duration)))
            .collect()
    }

    /// Drop finished tweens and ghosts. Returns true while anything is
    /// still running, i.e. the caller must schedule another frame.
    pub fn prune(&mut self, now: Instant, duration: Duration) -> bool {
        self.tweens
            .retain(|_, t| now.saturating_duration_since(t.start) < duration);
        self.ghosts
            .retain(|g| now.saturating_duration_since(g.start) < duration);
        !self.tweens.is_empty() || !self.ghosts.is_empty()
    }

    /// Fade-in factor (0..=1) for an overlay that is `shown` right now.
    /// The clock starts on the first shown frame and resets when hidden.
    /// The bool is true while the fade still needs more frames.
    pub fn fade(&mut self, key: FadeKey, shown: bool, now: Instant, duration: Duration) -> (f32, bool) {
        if !shown {
            self.fades.remove(&key);
            return (0.0, false);
        }
        let start = *self.fades.entry(key).or_insert(now);
        let t = progress(start, now, duration);
        (t, t < 1.0)
    }

    /// Forget every fade for which `keep` is false, so the next time that
    /// overlay shows it fades in again from zero.
    pub fn retain_fades(&mut self, keep: impl Fn(&FadeKey) -> bool) {
        self.fades.retain(|k, _| keep(k));
    }

    pub fn is_active(&self) -> bool {
        !self.tweens.is_empty() || !self.ghosts.is_empty()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    fn r(x: f32) -> PxRect {
        PxRect {
            x,
            y: 0.0,
            w: 100.0,
            h: 50.0,
        }
    }

    #[test]
    fn progress_bounds_and_monotonic() {
        let t0 = Instant::now();
        assert_eq!(progress(t0, t0, MS(150)), 0.0);
        assert_eq!(progress(t0, t0 + MS(150), MS(150)), 1.0);
        assert_eq!(progress(t0, t0 + MS(900), MS(150)), 1.0);
        let a = progress(t0, t0 + MS(40), MS(150));
        let b = progress(t0, t0 + MS(80), MS(150));
        assert!(0.0 < a && a < b && b < 1.0);
    }

    #[test]
    fn zero_duration_is_instant() {
        let t0 = Instant::now();
        assert_eq!(progress(t0, t0, MS(0)), 1.0);
    }

    #[test]
    fn tween_reaches_target_then_prunes() {
        let t0 = Instant::now();
        let mut anims = PaneAnimations::default();
        anims.start(1, r(0.0), r(200.0), t0, MS(150));
        let mid = anims.current(1, t0 + MS(60), MS(150)).unwrap();
        assert!(mid.x > 0.0 && mid.x < 200.0);
        let end = anims.current(1, t0 + MS(150), MS(150)).unwrap();
        assert_eq!(end, r(200.0));
        assert!(!anims.prune(t0 + MS(200), MS(150)));
        assert!(anims.current(1, t0 + MS(200), MS(150)).is_none());
    }

    #[test]
    fn retarget_starts_from_displayed_rect() {
        let t0 = Instant::now();
        let mut anims = PaneAnimations::default();
        anims.start(1, r(0.0), r(200.0), t0, MS(150));
        let shown = anims.current(1, t0 + MS(60), MS(150)).unwrap();
        anims.start(1, r(0.0), r(400.0), t0 + MS(60), MS(150));
        let now = anims.current(1, t0 + MS(60), MS(150)).unwrap();
        assert_eq!(now, shown);
    }

    #[test]
    fn unchanged_rect_starts_nothing() {
        let t0 = Instant::now();
        let mut anims = PaneAnimations::default();
        anims.start(1, r(5.0), r(5.0), t0, MS(150));
        assert!(!anims.is_active());
    }

    #[test]
    fn fade_starts_on_show_and_resets_on_hide() {
        let t0 = Instant::now();
        let mut anims = PaneAnimations::default();
        let k = FadeKey::LongPress(1);
        assert_eq!(anims.fade(k, true, t0, MS(150)), (0.0, true));
        let (mid, more) = anims.fade(k, true, t0 + MS(60), MS(150));
        assert!(mid > 0.0 && mid < 1.0 && more);
        assert_eq!(anims.fade(k, true, t0 + MS(200), MS(150)), (1.0, false));
        assert_eq!(anims.fade(k, false, t0 + MS(210), MS(150)), (0.0, false));
        assert_eq!(anims.fade(k, true, t0 + MS(220), MS(150)), (0.0, true));
    }

    #[test]
    fn ghost_fades_out() {
        let t0 = Instant::now();
        let mut anims = PaneAnimations::default();
        anims.add_ghost(r(0.0), t0);
        assert_eq!(anims.ghosts(t0, MS(150))[0].1, 1.0);
        assert_eq!(anims.ghosts(t0 + MS(150), MS(150))[0].1, 0.0);
        assert!(!anims.prune(t0 + MS(151), MS(150)));
    }
}
