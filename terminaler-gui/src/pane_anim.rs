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
    /// Set for two panes that traded places: each grows or shrinks straight
    /// into the other's rect over the swap duration.
    swap: bool,
}

/// A pane as painted in one frame.
#[derive(Debug, Clone, Copy)]
pub struct PaneSnap {
    pub rect: PxRect,
    pub bounds: [bool; 4],
    /// Background colour (linear RGBA), for the ghost of a closed pane.
    pub bg: [f32; 4],
}

/// What to do to one pane's quads this frame: every vertex maps to
/// `(x * sx + dx, y * sy + dy)`, then is clipped to `clip`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub dx: f32,
    pub dy: f32,
    /// Scale; 1.0 except for a lifted pane, whose drawing (laid out at its
    /// final size) is shrunk or stretched to the size it shows right now.
    pub sx: f32,
    pub sy: f32,
    /// Left, top, right, bottom; None when only translating.
    pub clip: Option<[f32; 4]>,
}

impl Transform {
    /// The same transform for vertices, whose origin is the window centre
    /// (`ox`, `oy` in window pixels) rather than the top-left corner. A pure
    /// shift is the same in both; a scale and a clip are not.
    pub fn to_vertex_space(&self, ox: f32, oy: f32) -> Transform {
        Transform {
            dx: ox * self.sx + self.dx - ox,
            dy: oy * self.sy + self.dy - oy,
            sx: self.sx,
            sy: self.sy,
            clip: self.clip.map(|[l, t, r, b]| [l - ox, t - oy, r - ox, b - oy]),
        }
    }

    /// Where `rect` lands under this transform.
    pub fn map(&self, rect: &PxRect) -> PxRect {
        PxRect {
            x: rect.x * self.sx + self.dx,
            y: rect.y * self.sy + self.dy,
            w: rect.w * self.sx,
            h: rect.h * self.sy,
        }
    }
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

/// How many of the four edges (left, top, right, bottom) differ by a pixel or more.
fn moved_edges(a: &PxRect, b: &PxRect) -> usize {
    let ea = [a.x, a.y, a.x + a.w, a.y + a.h];
    let eb = [b.x, b.y, b.x + b.w, b.y + b.h];
    (0..4).filter(|&i| (ea[i] - eb[i]).abs() >= 1.0).count()
}

fn rects_resized(a: &PxRect, b: &PxRect) -> bool {
    (a.w - b.w).abs() >= 1.0 || (a.h - b.h).abs() >= 1.0
}

/// Length of a swap, in milliseconds. The window sets it from
/// `pane_lift_ms`.
static LIFT_MS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(675);

pub fn set_lift_ms(ms: u32) {
    LIFT_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
}

fn lift_duration() -> Duration {
    Duration::from_millis(LIFT_MS.load(std::sync::atomic::Ordering::Relaxed) as u64)
}

/// CSS `cubic-bezier(x1, y1, x2, y2)` evaluated at time fraction `pos`.
/// `EasingFunction::as_bezier_array` yields exactly those four numbers, but
/// `evaluate_at_position` feeds them to a one-dimensional Bernstein sum
/// instead, so EaseIn and EaseInOut jump 42% at the first frame. Solve for
/// the curve parameter properly here.
fn css_ease(easing: EasingFunction, pos: f32) -> f32 {
    let [x1, y1, x2, y2] = easing.as_bezier_array();
    if pos <= 0.0 {
        return 0.0;
    }
    if pos >= 1.0 || matches!(easing, EasingFunction::Constant) {
        return 1.0;
    }
    let bez = |a: f32, b: f32, t: f32| 3.0 * (1.0 - t).powi(2) * t * a + 3.0 * (1.0 - t) * t * t * b + t.powi(3);
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if bez(x1, x2, mid) < pos {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bez(y1, y2, (lo + hi) / 2.0)
}

static EASING: parking_lot::Mutex<EasingFunction> = parking_lot::Mutex::new(EasingFunction::EaseOut);

/// Set the curve every animation uses; the window calls this from its config.
pub fn set_easing(easing: EasingFunction) {
    *EASING.lock() = easing;
}

/// Eased progress in 0..=1 of an animation that began at `start`.
pub fn progress(start: Instant, now: Instant, duration: Duration) -> f32 {
    if duration.is_zero() {
        return 1.0;
    }
    let elapsed = now.saturating_duration_since(start).as_secs_f32();
    let linear = (elapsed / duration.as_secs_f32()).clamp(0.0, 1.0);
    let easing = *EASING.lock();
    css_ease(easing, linear).clamp(0.0, 1.0)
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
        self.start_bounded(pane, from, to, [false; 4], false, now, duration);
    }

    fn start_bounded(
        &mut self,
        pane: PaneId,
        from: PxRect,
        to: PxRect,
        bounds: [bool; 4],
        swap: bool,
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
                    swap,
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
        if tween.swap {
            let dur = lift_duration();
            let raw = if dur.is_zero() {
                1.0
            } else {
                now.saturating_duration_since(tween.start).as_secs_f32() / dur.as_secs_f32()
            };
            let t = css_ease(EasingFunction::EaseInOut, raw.clamp(0.0, 1.0));
            return Some(tween.from.lerp(&tween.to, t));
        }
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
        self.tweens.retain(|_, t| {
            let dur = if t.swap { lift_duration() } else { duration };
            now.saturating_duration_since(t.start) < dur
        });
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
            //
            // The same panes before and after is a rearrangement (swap,
            // rotate, focus layout, dock). Every pane that relocated (more
            // than one edge moved) grows or shrinks straight into its new
            // rect, scaled, like a swapped pair; one that only moved one
            // edge is revealed as in a split.
            let rearranged = snaps.len() == self.last_rects.len()
                && snaps.iter().all(|(id, _)| self.last_rects.contains_key(id));
            for (id, snap) in snaps {
                match self.last_rects.get(id) {
                    Some(prev) => {
                        if rects_differ(&prev.rect, &snap.rect) {
                            let relocated = rearranged && moved_edges(&prev.rect, &snap.rect) > 1;
                            self.start_bounded(*id, prev.rect, snap.rect, snap.bounds, relocated, now, duration);
                        }
                    }
                    None => {
                        if let Some(from) = self.split_origin(snaps, &snap.rect, cell) {
                            self.start_bounded(*id, from, snap.rect, snap.bounds, false, now, duration);
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

    /// Record the current layout without animating the change. Used while a
    /// split is dragged: every cell of pointer travel changes the layout, and
    /// retargeting a reveal clip on each step leaves the pane blank.
    pub fn settle(&mut self, snaps: &[(PaneId, PaneSnap)], env: [f32; 5]) {
        self.tweens.clear();
        self.ghosts.clear();
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
        if tween.swap && final_rect.w >= 1.0 && final_rect.h >= 1.0 {
            // A swapped pane grows or shrinks on its way: scale its final
            // drawing to the rect it shows now, so a small pane swapped
            // into the main slot grows gradually instead of arriving big.
            // One factor for both axes keeps the text's proportions (the
            // main slot and a stack slot differ in shape, so separate
            // factors stretched it up to 70%); the larger one covers the
            // rect, and the overflow is cropped to it plus its gutters.
            let s = (cur.w / final_rect.w).max(cur.h / final_rect.h);
            return Some(Transform {
                dx: cur.x - final_rect.x * s,
                dy: cur.y - final_rect.y * s,
                sx: s,
                sy: s,
                clip: Some([
                    cur.x - margin.0,
                    cur.y - margin.1,
                    cur.x + cur.w + margin.0,
                    cur.y + cur.h + margin.1,
                ]),
            });
        }
        if tween.swap || !rects_resized(&tween.from, &tween.to) || moved_edges > 1 {
            return Some(Transform {
                dx: cur.x - final_rect.x,
                dy: cur.y - final_rect.y,
                sx: 1.0,
                sy: 1.0,
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
            sx: 1.0,
            sy: 1.0,
            clip: Some(clip),
        })
    }

    /// True while `id` is the shrinking half of a swap or relocation: its
    /// final layout is the small one, so it is drawn from its picture
    /// before the swap rather than magnifying that small layout.
    pub fn shrinking_swap(&self, id: PaneId) -> bool {
        self.tweens
            .get(&id)
            .map_or(false, |t| t.swap && t.to.w * t.to.h + 1.0 < t.from.w * t.from.h)
    }

    /// True while `id` is the growing half of a swap: it is painted above
    /// the others so it covers the pane shrinking out of its way.
    pub fn on_top(&self, id: PaneId) -> bool {
        self.tweens
            .get(&id)
            .map_or(false, |t| t.swap && t.to.w * t.to.h > t.from.w * t.from.h + 1.0)
    }

    /// Push every running animation's clock back by `by`, so a frame that
    /// took longer than one display refresh does not make the next frame
    /// jump ahead: the motion carries on from where it was last shown.
    pub fn delay(&mut self, by: Duration) {
        for tween in self.tweens.values_mut() {
            tween.start += by;
        }
        for ghost in &mut self.ghosts {
            ghost.start += by;
        }
        for start in self.fades.values_mut() {
            *start += by;
        }
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
    fn delay_holds_progress_across_a_slow_frame() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        a.add_ghost(r(0.0), [0.0; 4], t0);
        let key = FadeKey::SplitHover(0);
        a.fade(key, true, t0, MS(200));
        let before = a.ghosts(t0 + MS(50), MS(200))[0].2;
        // A 100 ms stall right after: delaying by it shows the same frame.
        a.delay(MS(100));
        let after = a.ghosts(t0 + MS(150), MS(200))[0].2;
        assert!((before - after).abs() < 1e-6);
        let (fade, _) = a.fade(key, true, t0 + MS(150), MS(200));
        assert!((fade - progress(t0, t0 + MS(50), MS(200))).abs() < 1e-6);
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
        // Same size both ways: a pure shift, cropped to the moving rect.
        assert_eq!((tr.dx, tr.sx, tr.sy), (-100.0, 1.0, 1.0));
        assert_eq!(tr.clip, Some([-5.0, -10.0, 105.0, 110.0]));
        let end = at(&a, 1, &swapped[0].1, t0 + MS(16 + 2000)).unwrap();
        assert_eq!(end.dx, 0.0);
    }

    #[test]
    fn swapped_panes_move_straight_without_a_detour() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        let before = [(1, snap(0.0, 100.0)), (2, snap(100.0, 50.0)), (3, snap(150.0, 50.0))];
        a.observe_frame(&before, ENV, CELL, t0, MS(150));
        let after = [(1, snap(150.0, 50.0)), (2, snap(100.0, 50.0)), (3, snap(0.0, 100.0))];
        a.observe_frame(&after, ENV, CELL, t0 + MS(16), MS(150));
        // Every frame, each edge sits between where it started and ended.
        for step in 0..=20 {
            let t = t0 + MS(16) + lift_duration().mul_f32(step as f32 / 20.0);
            let c = a.current(3, t, MS(150)).unwrap();
            assert!(c.x >= -0.01 && c.x <= 150.01, "left {} at {step}", c.x);
            assert!(c.x + c.w >= 99.99 && c.x + c.w <= 200.01, "right {} at {step}", c.x + c.w);
            assert_eq!(c.y, 0.0, "no vertical travel");
        }
    }

    #[test]
    fn a_reshuffle_relocates_every_moved_pane() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        // Focus layout shape: 1 is the main pane, 2 and 3 are stacked beside it.
        let before = [(1, snap(0.0, 100.0)), (2, snap(100.0, 50.0)), (3, snap(150.0, 50.0))];
        a.observe_frame(&before, ENV, CELL, t0, MS(150));
        // Pane 3 becomes main and the rest shift: no pair traded places, a
        // focus-layout rebuild. Every pane moves, scaled into its new rect.
        let after = [(1, snap(100.0, 50.0)), (2, snap(150.0, 50.0)), (3, snap(0.0, 100.0))];
        let start = t0 + MS(16);
        a.observe_frame(&after, ENV, CELL, start, MS(150));
        for (id, s) in &after {
            let tr = at(&a, *id, s, start).expect("relocated panes animate");
            assert_eq!(tr.sx, tr.sy, "a relocation is scaled uniformly");
        }
        let tr = at(&a, 3, &after[2].1, start).unwrap();
        let clip = tr.clip.expect("a relocation is cropped to the rect it shows");
        assert!((clip[2] - clip[0] - (50.0 + 2.0 * 5.0)).abs() < 0.5, "3 starts at its old width");
        assert!(a.on_top(3) && !a.on_top(1), "the growing pane covers the shrinking ones");
    }

    #[test]
    fn a_dock_reveals_one_edge_moves_and_relocates_the_docked_pane() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        let at_rect = |x: f32, y: f32, w: f32, h: f32| PaneSnap {
            rect: PxRect { x, y, w, h },
            bounds: [x == 0.0, y == 0.0, x + w >= 200.0, y + h >= 100.0],
            bg: [0.1, 0.1, 0.1, 1.0],
        };
        // 1 is main, 2 and 3 stacked on the right; 3 is docked under 1.
        let before = [
            (1, at_rect(0.0, 0.0, 100.0, 100.0)),
            (2, at_rect(100.0, 0.0, 100.0, 50.0)),
            (3, at_rect(100.0, 50.0, 100.0, 50.0)),
        ];
        a.observe_frame(&before, ENV, CELL, t0, MS(150));
        let after = [
            (1, at_rect(0.0, 0.0, 100.0, 50.0)),
            (2, at_rect(100.0, 0.0, 100.0, 100.0)),
            (3, at_rect(0.0, 50.0, 100.0, 50.0)),
        ];
        let start = t0 + MS(16);
        a.observe_frame(&after, ENV, CELL, start, MS(150));
        assert!(at(&a, 1, &after[0].1, start).unwrap().clip.is_some(), "1 lost its bottom half: revealed");
        assert!(at(&a, 2, &after[1].1, start).unwrap().clip.is_some(), "2 grew downwards: revealed");
        let moved = at(&a, 3, &after[2].1, start).unwrap();
        assert_eq!(moved.sx, moved.sy, "3 changed place: scaled move");
        assert!((moved.map(&after[2].1.rect).x - 100.0).abs() < 0.5, "3 starts where it was");
    }

    #[test]
    fn a_swap_among_unmoved_panes_animates_only_the_pair() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        let before = [(1, snap(0.0, 100.0)), (2, snap(100.0, 50.0)), (3, snap(150.0, 50.0))];
        a.observe_frame(&before, ENV, CELL, t0, MS(150));
        let after = [(1, snap(150.0, 50.0)), (2, snap(100.0, 50.0)), (3, snap(0.0, 100.0))];
        a.observe_frame(&after, ENV, CELL, t0 + MS(16), MS(150));
        assert!(a.current(1, t0 + MS(100), MS(150)).is_some());
        assert!(a.current(3, t0 + MS(100), MS(150)).is_some());
        assert!(a.current(2, t0 + MS(100), MS(150)).is_none(), "the bystander stays put");
    }

    #[test]
    fn a_swapped_small_pane_grows_and_the_main_pane_shrinks() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        // Focus layout: 1 is the main pane, 2 and 3 are stacked beside it.
        let before = [(1, snap(0.0, 100.0)), (2, snap(100.0, 50.0)), (3, snap(150.0, 50.0))];
        a.observe_frame(&before, ENV, CELL, t0, MS(150));
        let after = [(1, snap(150.0, 50.0)), (2, snap(100.0, 50.0)), (3, snap(0.0, 100.0))];
        let start = t0 + MS(16);
        a.observe_frame(&after, ENV, CELL, start, MS(150));
        let tr = |id: PaneId, s: &PaneSnap, t: Instant| at(&a, id, s, t).unwrap();
        let shown = |id: PaneId, s: &PaneSnap, t: Instant| tr(id, s, t).map(&s.rect);
        let close = |p: PxRect, q: PxRect| (p.x - q.x).abs() < 0.5 && (p.w - q.w).abs() < 0.5;
        // First frame: each covers exactly the rect it had, cropped to it,
        // with one scale factor for both axes.
        for (id, idx) in [(3, 2), (1, 0)] {
            let t = tr(id, &after[idx].1, start);
            assert_eq!(t.sx, t.sy, "pane {id} keeps its proportions");
            let s = t.map(&after[idx].1.rect);
            let b = before[idx].1.rect;
            assert!((s.x - b.x).abs() < 0.5 && (s.y - b.y).abs() < 0.5, "pane {id} anchored: {s:?}");
            assert!(s.w >= b.w - 0.5 && s.h >= b.h - 0.5, "pane {id} covers its rect: {s:?}");
            let clip = t.clip.expect("cropped to the rect it shows");
            assert!((clip[0] - (b.x - 5.0)).abs() < 0.5 && (clip[2] - (b.x + b.w + 5.0)).abs() < 0.5);
        }
        // Midway: what is visible (the crop, less its gutters) is between
        // the two sizes.
        let visible = |id: PaneId, idx: usize| {
            let c = tr(id, &after[idx].1, start + lift_duration().mul_f32(0.5)).clip.unwrap();
            c[2] - c[0] - 10.0
        };
        assert!(visible(3, 2) > 55.0 && visible(3, 2) < 95.0, "growing: {}", visible(3, 2));
        assert!(visible(1, 0) > 55.0 && visible(1, 0) < 95.0, "shrinking: {}", visible(1, 0));
        // Last frame: both are at their final rects.
        let end = start + lift_duration().mul_f32(0.999);
        assert!(close(shown(3, &after[2].1, end), after[2].1.rect));
        assert!(close(shown(1, &after[0].1, end), after[0].1.rect));
    }

    #[test]
    fn only_the_shrinking_pane_of_a_swap_uses_its_old_picture() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        let before = [(1, snap(0.0, 100.0)), (2, snap(100.0, 50.0)), (3, snap(150.0, 50.0))];
        a.observe_frame(&before, ENV, CELL, t0, MS(150));
        let after = [(1, snap(150.0, 50.0)), (2, snap(100.0, 50.0)), (3, snap(0.0, 100.0))];
        a.observe_frame(&after, ENV, CELL, t0 + MS(16), MS(150));
        assert!(a.shrinking_swap(1), "the old main pane shrinks");
        assert!(!a.shrinking_swap(3), "the incoming pane grows");
        assert!(!a.shrinking_swap(2), "the bystander is not animating");
    }

    #[test]
    fn only_the_growing_pane_of_a_swap_is_on_top() {
        let t0 = Instant::now();
        let mut a = PaneAnimations::default();
        let before = [(1, snap(0.0, 100.0)), (2, snap(100.0, 50.0)), (3, snap(150.0, 50.0))];
        a.observe_frame(&before, ENV, CELL, t0, MS(150));
        let after = [(1, snap(150.0, 50.0)), (2, snap(100.0, 50.0)), (3, snap(0.0, 100.0))];
        a.observe_frame(&after, ENV, CELL, t0 + MS(16), MS(150));
        assert!(a.on_top(3), "the incoming main pane grows over the other");
        assert!(!a.on_top(1), "the shrinking pane stays below");
        assert!(!a.on_top(2), "the bystander is not animating");
    }

    #[test]
    fn vertex_space_keeps_the_mapping_of_window_space() {
        let tr = Transform { dx: 30.0, dy: -4.0, sx: 0.5, sy: 2.0, clip: Some([10.0, 20.0, 110.0, 220.0]) };
        let (ox, oy) = (400.0, 300.0);
        let v = tr.to_vertex_space(ox, oy);
        // A point at window (x, y) is vertex (x - ox, y - oy); both routes agree.
        let (x, y) = (250.0, 120.0);
        assert!(((x - ox) * v.sx + v.dx - ((x * tr.sx + tr.dx) - ox)).abs() < 1e-3);
        assert!(((y - oy) * v.sy + v.dy - ((y * tr.sy + tr.dy) - oy)).abs() < 1e-3);
        assert_eq!(v.clip, Some([-390.0, -280.0, -290.0, -80.0]));
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
        // Pane 1 moves to the right and gets narrower: two edges change. A
        // third pane appears, so this is not a pure rearrangement.
        let after = [(1, snap(150.0, 50.0)), (2, snap(0.0, 150.0)), (3, snap(60.0, 10.0))];
        a.observe_frame(&after, ENV, CELL, t0 + MS(16), MS(150));
        let tr = at(&a, 1, &after[0].1, t0 + MS(16)).unwrap();
        assert_eq!(tr.clip, None, "no clip, or half the pane would be undrawn");
        assert_eq!(tr.dx, -150.0);
        let end = at(&a, 1, &after[0].1, t0 + MS(16 + 2000)).unwrap();
        assert_eq!((end.dx, end.clip), (0.0, None));
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
