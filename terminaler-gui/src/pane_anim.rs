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
    /// Which of the final rect's edges (left, top, right, bottom) lie on the
    /// window boundary; those are never clipped while they stay put.
    bounds: [bool; 4],
    start: Instant,
}

/// A pane as painted in one frame.
#[derive(Debug, Clone, Copy)]
pub struct PaneSnap {
    pub rect: PxRect,
    pub bounds: [bool; 4],
    /// Background colour (linear RGBA), for the ghost of a closed pane.
    pub bg: [f32; 4],
}

/// What to do to one pane's quads this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub dx: f32,
    pub dy: f32,
    /// Left, top, right, bottom; None when only translating.
    pub clip: Option<[f32; 4]>,
}

/// A pane that left the mux tree; its last rect fades out.
#[derive(Debug, Clone, Copy)]
pub struct Ghost {
    pub rect: PxRect,
    pub bg: [f32; 4],
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
    /// Final rect of every pane painted last frame, to spot layout changes.
    last_rects: HashMap<PaneId, PaneSnap>,
    /// Window/cell/sidebar geometry last frame; a change here moves every
    /// pane at once and must not be animated as a pane slide.
    last_env: Option<[f32; 5]>,
}

fn rects_differ(a: &PxRect, b: &PxRect) -> bool {
    (a.x - b.x).abs() >= 1.0 || (a.y - b.y).abs() >= 1.0 || rects_resized(a, b)
}

fn rects_resized(a: &PxRect, b: &PxRect) -> bool {
    (a.w - b.w).abs() >= 1.0 || (a.h - b.h).abs() >= 1.0
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
        self.start_bounded(pane, from, to, [false; 4], now, duration);
    }

    fn start_bounded(
        &mut self,
        pane: PaneId,
        from: PxRect,
        to: PxRect,
        bounds: [bool; 4],
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
                    bounds,
                    start: now,
                },
            );
        }
    }

    pub fn add_ghost(&mut self, rect: PxRect, bg: [f32; 4], now: Instant) {
        self.ghosts.push(Ghost {
            rect,
            bg,
            start: now,
        });
    }

    /// The rect to draw for `pane` now, or None when it is not animating.
    pub fn current(&self, pane: PaneId, now: Instant, duration: Duration) -> Option<PxRect> {
        let tween = self.tweens.get(&pane)?;
        let t = progress(tween.start, now, duration);
        Some(tween.from.lerp(&tween.to, t))
    }

    /// Ghost rects with their remaining opacity (1 -> 0).
    pub fn ghosts(&self, now: Instant, duration: Duration) -> Vec<(PxRect, [f32; 4], f32)> {
        self.ghosts
            .iter()
            .map(|g| (g.rect, g.bg, 1.0 - progress(g.start, now, duration)))
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

    /// Compare this frame's final pane rects with last frame's and start a
    /// tween for every pane that moved or resized, for every new pane (grown
    /// out of the split line it appeared at), and a ghost for every pane that
    /// is gone. `env` is the window/cell/sidebar geometry: when it changed
    /// everything moved together (window resize, font change, sidebar
    /// toggle), so nothing is animated. `cell` is the cell size in pixels.
    pub fn observe_frame(
        &mut self,
        snaps: &[(PaneId, PaneSnap)],
        env: [f32; 5],
        cell: (f32, f32),
        now: Instant,
        duration: Duration,
    ) {
        let env_changed = self.last_env != Some(env);
        if env_changed {
            self.tweens.clear();
            self.ghosts.clear();
        } else if snaps.iter().any(|(id, _)| self.last_rects.contains_key(id)) {
            // At least one pane survived, so this is a layout change inside
            // the tab and not a tab switch.
            for (id, snap) in snaps {
                match self.last_rects.get(id) {
                    Some(prev) => {
                        if rects_differ(&prev.rect, &snap.rect) {
                            self.start_bounded(*id, prev.rect, snap.rect, snap.bounds, now, duration);
                        }
                    }
                    None => {
                        if let Some(from) = self.split_origin(snaps, &snap.rect, cell) {
                            self.start_bounded(*id, from, snap.rect, snap.bounds, now, duration);
                        }
                    }
                }
            }
            let gone: Vec<(PxRect, [f32; 4])> = self
                .last_rects
                .iter()
                .filter(|(id, _)| !snaps.iter().any(|(sid, _)| sid == *id))
                .map(|(_, prev)| (prev.rect, prev.bg))
                .collect();
            for (rect, bg) in gone {
                self.add_ghost(rect, bg, now);
            }
        }
        self.last_env = Some(env);
        self.last_rects = snaps.iter().copied().collect();
    }

    /// Where a newly appeared pane `new` grows from: a zero-size rect on the
    /// split line it shares with the pane it was split off. Found by looking
    /// for a surviving pane whose previous rect is the union of its current
    /// rect and `new`.
    fn split_origin(&self, snaps: &[(PaneId, PaneSnap)], new: &PxRect, cell: (f32, f32)) -> Option<PxRect> {
        let (tx, ty) = cell;
        let near = |a: f32, b: f32, tol: f32| (a - b).abs() <= tol;
        for (id, now_snap) in snaps {
            let Some(prev) = self.last_rects.get(id) else {
                continue;
            };
            let p = now_snap.rect;
            let (l, t) = (p.x.min(new.x), p.y.min(new.y));
            let (r, b) = ((p.x + p.w).max(new.x + new.w), (p.y + p.h).max(new.y + new.h));
            let pr = prev.rect;
            if !(near(l, pr.x, tx) && near(t, pr.y, ty) && near(r, pr.x + pr.w, tx) && near(b, pr.y + pr.h, ty)) {
                continue;
            }
            let beside = near(new.y, p.y, ty) && near(new.h, p.h, ty);
            let above_below = near(new.x, p.x, tx) && near(new.w, p.w, tx);
            if beside {
                let x = if new.x >= p.x { new.x } else { new.x + new.w };
                return Some(PxRect { x, y: new.y, w: 0.0, h: new.h });
            }
            if above_below {
                let y = if new.y >= p.y { new.y } else { new.y + new.h };
                return Some(PxRect { x: new.x, y, w: new.w, h: 0.0 });
            }
        }
        None
    }

    /// How to draw pane `id` this frame, or None when it is not animating.
    /// A pane that only moved slides; one whose size changes keeps its final
    /// layout and is revealed through a growing clip rect, because the
    /// terminal grid is already at its final size.
    pub fn transform(
        &self,
        id: PaneId,
        final_rect: &PxRect,
        now: Instant,
        duration: Duration,
        margin: (f32, f32),
    ) -> Option<Transform> {
        let tween = self.tweens.get(&id)?;
        let cur = self.current(id, now, duration)?;
        let from = [tween.from.x, tween.from.y, tween.from.x + tween.from.w, tween.from.y + tween.from.h];
        let to = [tween.to.x, tween.to.y, tween.to.x + tween.to.w, tween.to.y + tween.to.h];
        // A reveal only reads well when one edge moves and the other three
        // stay put (split, close, drag-resize). When several edges move, the
        // pane is being relocated; a clip would leave part of it undrawn
        // until the tween ends, so slide it instead.
        let moved_edges = (0..4).filter(|&i| (from[i] - to[i]).abs() >= 1.0).count();
        if !rects_resized(&tween.from, &tween.to) || moved_edges > 1 {
            return Some(Transform {
                dx: cur.x - final_rect.x,
                dy: cur.y - final_rect.y,
                clip: None,
            });
        }
        let cur_e = [cur.x, cur.y, cur.x + cur.w, cur.y + cur.h];
        let mut clip = [0.0f32; 4];
        for i in 0..4 {
            let sign = if i < 2 { -1.0 } else { 1.0 };
            let m = if i % 2 == 0 { margin.0 } else { margin.1 };
            clip[i] = if (from[i] - to[i]).abs() >= 1.0 {
                cur_e[i]
            } else if tween.bounds[i] {
                cur_e[i] + sign * 1.0e7
            } else {
                cur_e[i] + sign * m
            };
        }
        Some(Transform {
            dx: 0.0,
            dy: 0.0,
            clip: Some(clip),
        })
    }

    pub fn has_tweens(&self) -> bool {
        !self.tweens.is_empty()
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

    const ENV: [f32; 5] = [800.0, 600.0, 10.0, 20.0, 0.0];
    const CELL: (f32, f32) = (10.0, 20.0);

    fn snap(x: f32, w: f32) -> PaneSnap {
        PaneSnap {
            rect: PxRect { x, y: 0.0, w, h: 100.0 },
            bounds: [x == 0.0, true, x + w >= 200.0, true],
            bg: [0.1, 0.1, 0.1, 1.0],
        }
    }

    fn at(a: &PaneAnimations, id: PaneId, snap: &PaneSnap, t: Instant) -> Option<Transform> {
        a.transform(id, &snap.rect, t, MS(150), (5.0, 10.0))
    }

    #[test]
    fn moved_pane_slides_without_clip() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        a.observe_frame(&[(1, snap(0.0, 100.0)), (2, snap(100.0, 100.0))], ENV, CELL, t0, MS(150));
        assert!(!a.has_tweens(), "first frame has nothing to compare");
        let swapped = [(1, snap(100.0, 100.0)), (2, snap(0.0, 100.0))];
        a.observe_frame(&swapped, ENV, CELL, t0 + MS(16), MS(150));
        let tr = at(&a, 1, &swapped[0].1, t0 + MS(16)).unwrap();
        assert_eq!((tr.dx, tr.clip), (-100.0, None));
        let end = at(&a, 1, &swapped[0].1, t0 + MS(16 + 150)).unwrap();
        assert_eq!(end.dx, 0.0);
    }

    #[test]
    fn env_change_suppresses_everything() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        a.observe_frame(&[(1, snap(0.0, 100.0))], ENV, CELL, t0, MS(150));
        a.observe_frame(&[(1, snap(50.0, 100.0))], [900.0, 600.0, 10.0, 20.0, 0.0], CELL, t0 + MS(16), MS(150));
        assert!(!a.has_tweens());
    }

    #[test]
    fn close_expands_survivor_and_ghosts_the_closed_pane() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        a.observe_frame(&[(1, snap(0.0, 100.0)), (2, snap(100.0, 100.0))], ENV, CELL, t0, MS(150));
        let after = [(1, snap(0.0, 200.0))];
        a.observe_frame(&after, ENV, CELL, t0 + MS(16), MS(150));
        let tr = at(&a, 1, &after[0].1, t0 + MS(16)).unwrap();
        let clip = tr.clip.expect("a resize is revealed through a clip");
        assert_eq!(clip[2], 100.0, "right edge starts at the old right edge");
        assert!(clip[0] < -1.0e6, "left edge is on the window boundary and stays open");
        let ghosts = a.ghosts(t0 + MS(16), MS(150));
        assert_eq!(ghosts.len(), 1);
        assert_eq!(ghosts[0].0.x, 100.0);
    }

    #[test]
    fn split_grows_the_new_pane_from_the_split_line() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        a.observe_frame(&[(1, snap(0.0, 200.0))], ENV, CELL, t0, MS(150));
        let after = [(1, snap(0.0, 100.0)), (2, snap(100.0, 100.0))];
        a.observe_frame(&after, ENV, CELL, t0 + MS(16), MS(150));
        let tr = at(&a, 2, &after[1].1, t0 + MS(16)).unwrap();
        let clip = tr.clip.unwrap();
        assert_eq!(clip[2], 100.0, "growing edge starts on the split line");
        assert_eq!(clip[0], 95.0, "fixed interior edge keeps its half-cell margin");
        let done = at(&a, 2, &after[1].1, t0 + MS(16 + 150)).unwrap();
        assert_eq!(done.clip.unwrap()[2], 200.0);
    }

    #[test]
    fn relocated_resized_pane_slides_instead_of_clipping() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        a.observe_frame(&[(1, snap(0.0, 100.0)), (2, snap(100.0, 100.0))], ENV, CELL, t0, MS(150));
        // Pane 1 moves to the right and gets narrower: two edges change.
        let after = [(1, snap(150.0, 50.0)), (2, snap(0.0, 150.0))];
        a.observe_frame(&after, ENV, CELL, t0 + MS(16), MS(150));
        let tr = at(&a, 1, &after[0].1, t0 + MS(16)).unwrap();
        assert_eq!(tr.clip, None, "no clip, or half the pane would be undrawn");
        assert_eq!(tr.dx, -150.0);
    }

    #[test]
    fn tab_switch_animates_nothing() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        a.observe_frame(&[(1, snap(0.0, 200.0))], ENV, CELL, t0, MS(150));
        a.observe_frame(&[(7, snap(0.0, 100.0)), (8, snap(100.0, 100.0))], ENV, CELL, t0 + MS(16), MS(150));
        assert!(!a.has_tweens() && a.ghosts(t0, MS(150)).is_empty());
    }

    #[test]
    fn ghost_fades_out() {
        let t0 = Instant::now();
        let mut anims = PaneAnimations::default();
        anims.add_ghost(r(0.0), [0.0; 4], t0);
        assert_eq!(anims.ghosts(t0, MS(150))[0].2, 1.0);
        assert_eq!(anims.ghosts(t0 + MS(150), MS(150))[0].2, 0.0);
        assert!(!anims.prune(t0 + MS(151), MS(150)));
    }
}
