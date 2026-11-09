//! Hold a pane's last good picture across a divider release.
//!
//! When a divider is released, every pane it resizes is redrawn twice by the
//! program behind it when that program is a tmux client running a TUI such
//! as Claude Code: tmux answers the resize within a few milliseconds with the
//! old screen rewrapped to the new width, and the TUI's own redraw follows
//! 200-300 ms later. The rewrapped picture in between reads as a flash. A
//! held pane keeps showing the picture it had before the release, mapped to
//! its new rect, until its output settles or the hold times out.
//!
//! Pure state: the window keeps the snapshot quads; this decides when each
//! hold ends, so the rule is unit tested.
use crate::pane_anim::PxRect;
use mux::pane::PaneId;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Output that starts this long after the pane's last resize is the program's
/// own redraw, not tmux's immediate answer to the resize.
const LATE_OUTPUT: Duration = Duration::from_millis(100);
/// The redraw is over once the pane has been quiet this long after it.
const QUIET: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    /// The program redrew after the resize and then went quiet.
    Settled,
    /// The hold reached its maximum length.
    Timeout,
}

#[derive(Debug, Clone, Copy)]
struct Hold {
    /// The pane's content rect when its picture was captured.
    rect: PxRect,
    started: Instant,
    resized_at: Instant,
    last_output: Option<Instant>,
    /// Output arrived at least `LATE_OUTPUT` after the last resize.
    late: bool,
}

#[derive(Default)]
pub struct PaneHolds {
    holds: HashMap<PaneId, Hold>,
}

impl PaneHolds {
    pub fn start(&mut self, pane: PaneId, rect: PxRect, now: Instant) {
        self.holds.insert(
            pane,
            Hold {
                rect,
                started: now,
                resized_at: now,
                last_output: None,
                late: false,
            },
        );
    }

    /// The pane's pty was resized again (the font fit after the layout
    /// resize): tmux answers this one too, so restart the late-output clock.
    pub fn note_resize(&mut self, pane: PaneId, now: Instant) {
        if let Some(h) = self.holds.get_mut(&pane) {
            h.resized_at = now;
            h.late = false;
        }
    }

    pub fn note_output(&mut self, pane: PaneId, now: Instant) {
        if let Some(h) = self.holds.get_mut(&pane) {
            if now.saturating_duration_since(h.resized_at) >= LATE_OUTPUT {
                h.late = true;
            }
            h.last_output = Some(now);
        }
    }

    /// The captured rect of a pane that is held, or None.
    pub fn rect(&self, pane: PaneId) -> Option<PxRect> {
        self.holds.get(&pane).map(|h| h.rect)
    }

    pub fn is_empty(&self) -> bool {
        self.holds.is_empty()
    }

    pub fn panes(&self) -> Vec<PaneId> {
        self.holds.keys().copied().collect()
    }

    pub fn remove(&mut self, pane: PaneId) {
        self.holds.remove(&pane);
    }

    pub fn clear(&mut self) {
        self.holds.clear();
    }

    /// End every hold whose time has come. Returns each ended pane with why
    /// and how long it was held.
    pub fn release_due(&mut self, now: Instant, max: Duration) -> Vec<(PaneId, Release, Duration)> {
        let mut ended = vec![];
        self.holds.retain(|id, h| {
            let held = now.saturating_duration_since(h.started);
            let reason = if h.late
                && h.last_output
                    .map_or(false, |t| now.saturating_duration_since(t) >= QUIET)
            {
                Some(Release::Settled)
            } else if held >= max {
                Some(Release::Timeout)
            } else {
                None
            };
            match reason {
                Some(r) => {
                    ended.push((*id, r, held));
                    false
                }
                None => true,
            }
        });
        ended
    }
}

/// Where to draw a held picture captured at `from` so it fills `to`: scaled
/// by the same factor on both axes (text keeps its shape), anchored at the
/// top left and clipped to `to`, in window coordinates.
pub fn fit_into(from: &PxRect, to: &PxRect) -> crate::pane_anim::Transform {
    let s = if from.w >= 1.0 && from.h >= 1.0 {
        (to.w / from.w).min(to.h / from.h)
    } else {
        1.0
    };
    crate::pane_anim::Transform {
        dx: to.x - from.x * s,
        dy: to.y - from.y * s,
        sx: s,
        sy: s,
        clip: Some([to.x, to.y, to.x + to.w, to.y + to.h]),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;
    const MAX: Duration = Duration::from_millis(400);

    fn r(w: f32) -> PxRect {
        PxRect { x: 0.0, y: 0.0, w, h: 100.0 }
    }

    #[test]
    fn tmux_answer_alone_does_not_end_the_hold() {
        let t0 = Instant::now();
        let mut h = PaneHolds::default();
        h.start(1, r(200.0), t0);
        // tmux answers the resize at once, then nothing for a while.
        h.note_output(1, t0 + MS(2));
        assert!(h.release_due(t0 + MS(80), MAX).is_empty(), "an early burst is tmux, not the redraw");
        assert!(h.release_due(t0 + MS(300), MAX).is_empty());
        let ended = h.release_due(t0 + MS(400), MAX);
        assert_eq!(ended, vec![(1, Release::Timeout, MS(400))]);
        assert!(h.is_empty());
    }

    #[test]
    fn a_late_redraw_ends_the_hold_once_quiet() {
        let t0 = Instant::now();
        let mut h = PaneHolds::default();
        h.start(1, r(200.0), t0);
        h.note_output(1, t0 + MS(2));
        // The TUI redraws in a burst from 220 to 260 ms.
        for ms in [220, 235, 250, 260] {
            h.note_output(1, t0 + MS(ms));
        }
        assert!(h.release_due(t0 + MS(290), MAX).is_empty(), "still within the quiet window");
        assert_eq!(h.release_due(t0 + MS(311), MAX), vec![(1, Release::Settled, MS(311))]);
    }

    #[test]
    fn a_second_resize_restarts_the_late_clock() {
        let t0 = Instant::now();
        let mut h = PaneHolds::default();
        h.start(1, r(200.0), t0);
        // The font fit resizes the pty again 80 ms in; tmux answers at 82.
        h.note_resize(1, t0 + MS(80));
        h.note_output(1, t0 + MS(82));
        assert!(h.release_due(t0 + MS(150), MAX).is_empty(), "tmux's answer to the refit is not late");
        h.note_output(1, t0 + MS(250));
        assert_eq!(h.release_due(t0 + MS(305), MAX), vec![(1, Release::Settled, MS(305))]);
    }

    #[test]
    fn panes_end_independently_and_unknown_panes_are_ignored() {
        let t0 = Instant::now();
        let mut h = PaneHolds::default();
        h.start(1, r(200.0), t0);
        h.start(2, r(100.0), t0);
        h.note_output(9, t0 + MS(200));
        h.note_output(1, t0 + MS(200));
        let ended = h.release_due(t0 + MS(260), MAX);
        assert_eq!(ended, vec![(1, Release::Settled, MS(260))]);
        assert_eq!(h.panes(), vec![2]);
        assert_eq!(h.rect(2), Some(r(100.0)));
    }

    #[test]
    fn fit_keeps_the_shape_and_clips_to_the_new_rect() {
        let from = PxRect { x: 100.0, y: 50.0, w: 400.0, h: 200.0 };
        // Narrower and as tall: scaled down uniformly by the width.
        let to = PxRect { x: 300.0, y: 50.0, w: 200.0, h: 200.0 };
        let tr = fit_into(&from, &to);
        assert_eq!((tr.sx, tr.sy), (0.5, 0.5));
        let shown = tr.map(&from);
        assert_eq!((shown.x, shown.y, shown.w, shown.h), (300.0, 50.0, 200.0, 100.0));
        assert_eq!(tr.clip, Some([300.0, 50.0, 500.0, 250.0]));
        // Wider and as tall: not stretched, the extra width stays empty.
        let wide = PxRect { x: 0.0, y: 50.0, w: 800.0, h: 200.0 };
        let tr = fit_into(&from, &wide);
        assert_eq!((tr.sx, tr.sy), (1.0, 1.0));
        assert_eq!(tr.map(&from).x, 0.0);
    }
}
