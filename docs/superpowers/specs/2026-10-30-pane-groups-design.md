# Pane groups and stacks

Status: draft for review, 2026-10-30. Nothing is implemented.

## Purpose

Vilius runs a Claude Code pane next to a plain shell pane and wants the two to
behave as one pane: focusing the pair brings both into view together. Several
such pairs must be able to share one layout slot, and focusing one swaps it in
for the others.

Success looks like this:

- Two or more panes can be bound into a **group** that is focused, hidden and
  shown as one unit.
- Several units can share one **stack** slot. Exactly one unit is visible; the
  rest keep running hidden.
- Keyboard actions come first. The sidebar is a second front end over the same
  actions.

Provenance: the purpose, the shared-slot stack semantics (option 1) and
"keyboard first, then sidebar" are Vilius's stated choices (2026-10-30).
Everything else below is a design choice made in this document.

## Non-goals

- No new layout engine and no change to the `Tree` leaf type.
- No default key bindings. Vilius picks keys in the config.
- No Windows validation (the Windows build is defunct, see CLAUDE.md).
- No drag-to-group in phase 1.

## Terms

| Term | Meaning |
|---|---|
| Unit | A single pane, or a group. Every pane is a unit of one. |
| Group | Two or more panes treated as one unit. |
| Stack | The ordered units that share one slot, with one active unit. |
| Slot | The layout rectangle a stack occupies. |

## Approach

Overlay the model on the existing split tree (`mux/src/tab.rs`, `Tree =
bintree::Tree<Arc<dyn Pane>, SplitDirectionAndSize>`). Rejected alternatives:

- A new leaf type (`Single` or `Stack`): about 53 `Tree::Leaf/Node/Empty` sites
  in `tab.rs`, plus `session_state.rs` and the GUI. Too large for the gain.
- A composite `Pane` object: the `Pane` trait is built around one terminal's
  lines and cursor, so a composite would fake that interface.

The overlay works because `TabInner.hidden` already hides whole subtrees. When
one side of a split is entirely hidden, the other side gets the full parent
size (`apply_sizes_from_splits_with_hidden`, `is_subtree_hidden`, `tab.rs`
around lines 558-731). Swapping the visible unit in a stack is a change to the
`hidden` set and needs no layout code. `hidden` is already written to session
state (`convert_tree_to_session_layout`), which is the basis for persistence.

## Data model

`TabInner` gains:

```rust
groups: Vec<PaneGroup>,   // PaneGroup { id: GroupId, members: Vec<PaneId>, last_active: PaneId }
stacks: Vec<PaneStack>,   // PaneStack { units: Vec<Unit>, active: usize }
// enum Unit { Pane(PaneId), Group(GroupId) }
```

Invariants (checked by a `debug_assert`-style validator run after every
mutation, and by tests):

1. A group has at least two members. A group that drops to one member dissolves.
2. A group's member panes form one contiguous subtree of the split tree.
3. All units of a stack sit under one parent split node, and every pane of
   every non-active unit is in `hidden`.
4. A pane belongs to at most one group. A unit belongs to at most one stack.
5. A tab always has at least one visible pane (the existing
   `toggle_pane_hidden` rule, `tab.rs` around line 1288, is kept).

A unit that is not in any multi-unit stack needs no stack entry.

## Actions

New `KeyAssignment` variants in `config/src/keyassignment.rs`, unbound by
default:

| Action | Effect |
|---|---|
| `PaneGroup(direction)` | Merge the active unit with its neighbour in that direction into one group. If the neighbour is a group, the active unit joins it. |
| `PaneUngroup` | Dissolve the active group into plain panes. |
| `PaneStackJoin(direction)` | Move the active unit into the neighbour's slot, as a new stack member. The joined unit becomes active and the previous one is hidden. |
| `PaneStackCycle(next or prev)` | Show the next or previous unit in the active stack. Hide the current one. |

Example: select the Claude pane, run `PaneGroup(Right)` to bind it to the shell,
then repeat for a second pair and run `PaneStackJoin` to make them share a slot.

Each action is also a `CommandDef` in `terminaler-gui/src/commands.rs` so it
appears in the command palette, as `TogglePaneHidden` does.

## Focus and navigation

- Focusing a group focuses its `last_active` member.
- Directional navigation (`get_pane_direction`, `tab.rs` line 1015) ignores
  hidden panes and never lands in a hidden unit.
- A hidden unit is reached by `PaneStackCycle` or, in phase 2, the sidebar.
- Split, resize and rotate act on visible panes only. Rotate on a stack's
  parent node is a no-op.

## Lifecycle

- A group left with one member dissolves into a plain pane.
- Closing the active unit of a stack reveals the next unit.
- Closing a group closes all its members, after the existing confirm prompt.
  Rationale: a half-closed pair is the case the feature exists to avoid.
- Zoom on a single pane inside a group behaves as today.
- Killing the last pane of a hidden unit removes the unit from its stack.

## Persistence

Group and stack state is saved with the session layout and restored with it.
`SessionLayout` entries (`mux/src/session_state.rs`) gain optional group id and
stack fields; files without them load as before (default `None`).

## Sidebar (phase 2)

- A group tile shows its members nested under one header.
- The active unit of a stack carries a marker; hidden units are listed dimmed.
- Clicking a hidden unit's tile cycles it into view (calls the same code as
  `PaneStackCycle`).
- Drag-to-group reuses `PaneGroup`.
- Linux renders this in `terminaler-gui/src/termwindow/render/tab_sidebar.rs`;
  the Windows WebView sidebar is out of scope.
- Any sidebar change must call both `invalidate_tab_sidebar()` and
  `context.invalidate()` (see memory: cache clear and repaint are two calls).

## Open risks (verify before or during phase 1)

1. **Remote mux domains.** The hidden-pane API is consumed directly by the GUI
   (`terminaler-gui/src/termwindow/mod.rs`, `session.rs`, `commands.rs`) through
   `Tab`. Verified: none of `codec/` mentions it. So groups work for the local
   domain, but a mux-server attached over a socket would not carry group state.
   Decision: phase 1 supports the local domain only; remote support needs
   codec changes and is a separate task. This must be confirmed against how
   homepc runs the GUI (assumed: local domain with ssh+tmux panes).
2. **Tree operations that assume every node is a plain split.** `rotate_*`,
   pane removal and swap in `tab.rs` need guards for a stack's parent node.
   Unknown until read in full; the first plan task is an audit of these.
3. **Animation interplay.** Pane animations (see memory: pane animations) fade
   and slide on hide/show. A stack swap must not retrigger the closed-pane
   ghost for a pane that is only hidden.

## Testing

- Unit tests in `mux/src/tab.rs` for each invariant: group, ungroup, join,
  cycle, close of active, close of last member, session round-trip.
- Headless end-to-end on devbox with `terminaler-mux-server` and
  `terminaler cli`, as described in CLAUDE.md.
- Visual check on homepc: group a Claude pane with a shell, stack two pairs,
  cycle, restart the GUI and confirm restore. Done by Vilius.
- Each test must fail with the feature reverted (construct the failing
  condition, per memory).

## Phasing

1. Model, invariants, four actions, command palette entries, persistence, tests.
2. Sidebar rendering and click-to-cycle.
3. Drag-to-group, if wanted after using 1 and 2.
