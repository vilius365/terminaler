//! Stable per-agent accent colors.
//!
//! Each Claude agent is keyed by its worktree (the `claude_worktree` user
//! var). We map that string deterministically to a color from a curated
//! palette so the same agent always renders with the same hue — letting you
//! tell tiled agents apart at a glance. This is an *identity* signal,
//! orthogonal to the *status* color (working/waiting/idle/error).

use window::color::LinearRgba;

/// Curated palette of visually distinct, readable accent hues.
/// Kept deliberately small and spread across the hue wheel so adjacent
/// agents are easy to distinguish.
const AGENT_PALETTE: &[(f32, f32, f32)] = &[
    (0.302, 0.620, 1.000), // blue
    (0.667, 0.475, 0.965), // purple
    (0.180, 0.800, 0.745), // teal
    (0.965, 0.443, 0.737), // pink
    (0.984, 0.737, 0.176), // amber
    (0.408, 0.776, 0.310), // green
    (0.380, 0.745, 0.984), // sky
    (0.965, 0.529, 0.310), // orange
    (0.745, 0.776, 0.176), // lime
    (0.580, 0.639, 0.722), // slate
];

/// FNV-1a hash — small and fully deterministic across toolchains/platforms
/// (unlike `DefaultHasher`, whose output is not contractually stable), so a
/// given worktree always lands on the same palette entry.
fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Palette index for a seed. Stable for a given seed.
pub fn agent_color_index(seed: &str) -> usize {
    (fnv1a(seed) % AGENT_PALETTE.len() as u64) as usize
}

/// Stable accent color for an agent identified by `seed` (its worktree).
/// `active` controls brightness: inactive panes/tabs are dimmed to 70% with
/// reduced alpha, matching the existing status-accent convention.
pub fn agent_accent_color(seed: &str, active: bool) -> LinearRgba {
    let (r, g, b) = AGENT_PALETTE[agent_color_index(seed)];
    if active {
        LinearRgba::with_components(r, g, b, 1.0)
    } else {
        LinearRgba::with_components(r * 0.7, g * 0.7, b * 0.7, 0.6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_for_same_seed() {
        assert_eq!(
            agent_color_index("repo-worktrees/agent-foo"),
            agent_color_index("repo-worktrees/agent-foo")
        );
        // Determinism across calls for the full color too.
        assert_eq!(
            agent_accent_color("x", true),
            agent_accent_color("x", true)
        );
    }

    #[test]
    fn index_in_bounds() {
        for seed in ["", "a", "agent/foo", "/very/long/worktree/path-123", "🌟"] {
            assert!(agent_color_index(seed) < AGENT_PALETTE.len());
        }
    }

    #[test]
    fn distinct_seeds_spread_across_palette() {
        use std::collections::HashSet;
        let mut indices = HashSet::new();
        for i in 0..50 {
            indices.insert(agent_color_index(&format!("agent/{i}")));
        }
        // 50 seeds should touch most of a 10-color palette, not collapse to one.
        assert!(
            indices.len() >= AGENT_PALETTE.len() / 2,
            "poor distribution: only {} of {} palette slots used",
            indices.len(),
            AGENT_PALETTE.len()
        );
    }

    #[test]
    fn inactive_is_dimmed() {
        let active = agent_accent_color("seed", true);
        let inactive = agent_accent_color("seed", false);
        assert!(inactive.0 < active.0 || active.0 == 0.0);
        assert!(inactive.3 < active.3, "inactive alpha should be reduced");
    }
}
