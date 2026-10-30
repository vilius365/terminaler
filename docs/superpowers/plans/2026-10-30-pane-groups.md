# Pane Groups and Stacks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let two or more panes be bound into a group that focuses, hides and shows as one unit, and let several units share one layout slot (a stack) with one visible at a time.

**Architecture:** A pure data model (`GroupState`) overlays the existing split tree in `mux/src/tab.rs`. Inactive units' panes sit in `TabInner.hidden`, and the existing subtree-aware hidden layout does the swap. Four new unbound `KeyAssignment`s drive it.

**Tech Stack:** Rust, `mux` crate (tab model), `config` (actions), `terminaler-gui` (dispatch, commands). Tests use `FakePane` in `mux/src/tab.rs`'s test module.

**Spec:** `docs/superpowers/specs/2026-10-30-pane-groups-design.md`. This plan covers **phase 1 only** (model, actions, persistence, tests). Phase 2 (sidebar) gets its own plan after phase 1 lands.

## Global Constraints

- No default key bindings for the new actions (spec: Non-goals).
- Local mux domain only; no `codec/` changes (spec: Open risks 1).
- No tree leaf type change: `Tree = bintree::Tree<Arc<dyn Pane>, SplitDirectionAndSize>` stays.
- Windows build is defunct: do not build, stage or validate it.
- Style: rustfmt defaults, `anyhow`, `log`, `parking_lot::Mutex`.
- Build and test with `~/.cargo/bin/cargo` (bare `cargo` is not on PATH). Redirect output to a log file and read the exit status of cargo itself: `~/.cargo/bin/cargo test -p mux --lib <filter> > $TMPDIR/t.log 2>&1; echo $?`. Never trust `| tail`.
- Commit straight to `main`, never push. End commit messages with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01YaHLXmxc8SM8Ua6n4RRcdg`.
- Each new test must fail with its feature reverted (stash the implementation hunk, run, restore).
- Invariants 1-5 from the spec ("Data model") hold after every mutation.

## Review Focus

Spec-implied cases no task's main tests would otherwise hit, most likely first:

1. Closing or killing the last pane of a **hidden** unit while another unit is active (no focus change, stack shrinks, no panic). Task 4.
2. `PaneStackCycle` on a stack of one unit, or on a pane in no stack: no-op, returns false. Task 3.
3. Joining a unit into a slot when it is the only visible unit in the tab (invariant 5). Task 3.
4. A session file saved before this feature (no group fields) loads with no groups. Task 5.
5. `rotate_*`, `swap_panes`, `focus_layout` on a tab that has a stack must not break invariants. Task 4.

## File Structure

| File | Responsibility |
|---|---|
| `mux/src/pane_group.rs` (new) | `GroupState` data, unit lookup, validation. No `Tab` dependency except reading a `Tree`. |
| `mux/src/tab.rs` | `Tab`/`TabInner` group operations, hidden-set swaps, lifecycle hooks, tests. |
| `mux/src/session_state.rs` | Optional `group`/`stack` fields on `PaneLayoutNode::Pane`. |
| `config/src/keyassignment.rs` | Four new `KeyAssignment` variants. |
| `terminaler-gui/src/commands.rs`, `termwindow/mod.rs`, `session.rs` | Command palette entries, dispatch, close-group, restore. |

---

### Task 1: Group state model and validator

**Files:**
- Create: `mux/src/pane_group.rs`
- Modify: `mux/src/lib.rs` (add `pub mod pane_group;` beside `pub mod tab;`)

**Interfaces:**
- Produces:
  - `pub type GroupId = u32;`
  - `pub enum Unit { Pane(PaneId), Group(GroupId) }` (derive `Clone, Debug, PartialEq, Eq`)
  - `pub struct GroupInfo { pub id: GroupId, pub members: Vec<PaneId>, pub last_active: PaneId }`
  - `pub struct PaneStack { pub units: Vec<Unit>, pub active: usize }`
  - `pub struct GroupState { pub groups: Vec<GroupInfo>, pub stacks: Vec<PaneStack> }` with `Default`
  - `GroupState::unit_of(&self, pane: PaneId) -> Unit`
  - `GroupState::unit_panes(&self, unit: &Unit) -> Vec<PaneId>`
  - `GroupState::stack_of(&self, unit: &Unit) -> Option<usize>` (index into `stacks`)
  - `GroupState::create_group(&mut self, members: Vec<PaneId>) -> GroupId` (ids never reused within a state)
  - `GroupState::dissolve(&mut self, id: GroupId)`
  - `GroupState::validate(&self, tree: &Tree, hidden: &HashSet<PaneId>) -> Result<(), String>`
- Consumes: `crate::tab::Tree`, `crate::pane::PaneId`.

- [ ] **Step 1: Write failing tests** in `pane_group.rs`'s own `#[cfg(test)]` module, building small `Tree`s from a local test pane type or by importing `FakePane` made `pub(crate)` under `#[cfg(test)]` in `tab.rs`. Tests: `group_of_two_adjacent_leaves_validates`, `group_with_one_member_fails_validation`, `group_split_across_subtrees_fails_validation` (members 1 and 3 in `((1,2),3)`), `pane_in_two_groups_fails_validation`, `stack_with_two_visible_units_fails_validation`, `stack_with_no_visible_unit_fails_validation`, `unit_of_returns_group_for_member_and_pane_otherwise`.
- [ ] **Step 2: Run** `~/.cargo/bin/cargo test -p mux --lib pane_group > $TMPDIR/t.log 2>&1; echo $?`. Expected: non-zero (module missing).
- [ ] **Step 3: Implement** the interface above. Contiguity check: collect every subtree's leaf-id set from the tree; a group is contiguous when its member set equals one of them. A stack's units must share one parent: the union of the units' panes equals one subtree's leaf set.
- [ ] **Step 4: Run** the same command. Expected: exit 0, all seven tests pass.
- [ ] **Step 5: Commit** `Pane groups: state model and validator`.

---

### Task 2: Unit-level docking primitive (highest risk)

`dock_pane` (`tab.rs` ~947) moves one pane against one target pane. A group needs a whole subtree docked against a whole subtree, so a stack's units end up under one parent.

**Files:**
- Modify: `mux/src/tab.rs` (`TabInner`, `Tab`; add `groups: GroupState` field to `TabInner` and its constructor)
- Test: `mux/src/tab.rs` test module

**Interfaces:**
- Consumes: `GroupState::validate` (Task 1).
- Produces:
  - `Tab::dock_unit(&self, source: &[PaneId], target: &[PaneId], request: SplitRequest) -> bool`
  - `TabInner::dock_unit` with the same arguments.
  - Behaviour: removes `source` panes from the tree (no kill), finds the subtree whose leaf set equals `target`, and replaces it with a split node of `[old subtree, new subtree of source]` per `request`. The source subtree is the source panes chained in a horizontal split in the order given. Returns false and leaves the tab untouched when a pane is missing, `source` and `target` overlap, the tab is zoomed, or `target` is not a whole subtree.

- [ ] **Step 1: Write failing tests**: `dock_unit_places_two_panes_beside_a_single_pane` (tree `(1,(2,3))`, dock `[2,3]` to `[1]`: result tree has the leaf set `{1}` and `{2,3}` as siblings), `dock_unit_rejects_target_that_is_not_a_whole_subtree` (target `[2]` of `(1,(2,3))` where `[2]` is a leaf is fine, but target `[1,2]` is not a subtree: returns false), `dock_unit_rejects_overlap`, `dock_unit_keeps_all_panes_alive_and_sized` (every pane in `iter_panes()` has width and height above 0).
- [ ] **Step 2: Run** `... test -p mux --lib dock_unit`. Expected: non-zero (not defined).
- [ ] **Step 3: Implement** `TabInner::dock_unit`. Use `remove_pane` for each source pane (it keeps the pane alive, `tab.rs` ~2335), then rebuild the target subtree with a new `Tree::Node`; reuse the size-splitting logic `split_and_insert` and `dock_pane` use rather than writing new size math. Re-run `self.resize(self.size)` at the end.
- [ ] **Step 4: Run** the same command. Expected: exit 0.
- [ ] **Step 5: Commit** `Tab: dock a whole unit against another subtree`.

---

### Task 3: Group, ungroup, join and cycle operations

**Files:**
- Modify: `mux/src/tab.rs`
- Test: `mux/src/tab.rs` test module

**Interfaces:**
- Consumes: `GroupState` (Task 1), `Tab::dock_unit` (Task 2), `config::keyassignment::PaneDirection`, existing `TabInner::get_pane_direction(&mut self, direction: PaneDirection, ignore_zoom: bool) -> Option<usize>` and `iter_panes_ignoring_zoom`.
- Produces on `Tab` (each forwards to `TabInner`):
  - `group_panes(&self, direction: PaneDirection) -> bool`
  - `ungroup_active(&self) -> bool`
  - `stack_join(&self, direction: PaneDirection) -> bool`
  - `stack_cycle(&self, direction: PaneDirection) -> bool` (only `Next` and `Prev` act; others return false)
  - `unit_panes_of(&self, pane: PaneId) -> Vec<PaneId>`
  - Private `TabInner::set_unit_hidden(&mut self, unit: &Unit, hidden: bool)` that edits `self.hidden` directly for all the unit's panes. It must not call `toggle_pane_hidden`, which hides one pane at a time and refuses to hide the last visible pane.

- [ ] **Step 1: Write failing tests** (each also asserts `groups.validate(...)` is `Ok` afterwards): `group_panes_merges_active_pane_with_right_neighbour`, `group_panes_into_existing_group_extends_it`, `group_panes_returns_false_with_no_neighbour`, `ungroup_active_restores_plain_panes`, `stack_join_hides_previous_unit_and_shows_joined_one`, `stack_join_hidden_panes_leave_exactly_one_visible_unit`, `stack_cycle_next_swaps_visible_unit_and_wraps`, `stack_cycle_on_single_unit_is_noop_false` (Review Focus 2), `stack_join_when_only_visible_unit_is_refused_or_keeps_one_visible` (Review Focus 3: assert at least one pane visible after).
- [ ] **Step 2: Run** `... test -p mux --lib group`. Expected: non-zero.
- [ ] **Step 3: Implement.**
  - `group_panes`: find the neighbour pane via `get_pane_direction`, resolve both to units, `dock_unit` them adjacent if not already a shared subtree, then `create_group` with the union (or extend the neighbour's group).
  - `stack_join`: resolve the neighbour unit, `dock_unit(active_unit_panes, neighbour_unit_panes, request)`, add both to one `PaneStack` (or extend the neighbour's), set the joined unit active, hide the other units via `set_unit_hidden`.
  - `stack_cycle`: advance `PaneStack.active`, hide the old unit, show the new, set the active pane to the new unit's `last_active`, then `self.resize(self.size)` and notify `MuxNotification::TabResized`.
  - All four call `GroupState::validate` under `debug_assert!` after mutating.
- [ ] **Step 4: Run** the same command. Expected: exit 0.
- [ ] **Step 5: Commit** `Tab: group, ungroup, stack join and cycle`.

---

### Task 4: Lifecycle, focus and tree-operation guards

**Files:**
- Modify: `mux/src/tab.rs` (`remove_pane_if` ~2343, `set_active_pane` ~2489, `rotate_*` ~1354/1385, `swap_panes`, `focus_layout`)
- Test: `mux/src/tab.rs` test module

**Interfaces:**
- Consumes: Task 3 operations.
- Produces: `GroupState::remove_pane(&mut self, pane: PaneId)` that drops the pane, dissolves a group left with one member, removes the unit from its stack, and reports whether the removed pane's unit was the stack's active unit.

- [ ] **Step 1: Write failing tests**: `removing_one_of_two_group_members_dissolves_group`, `closing_active_unit_in_stack_reveals_next_unit`, `closing_last_pane_of_hidden_unit_keeps_active_unit_focus` (Review Focus 1), `killing_group_member_updates_last_active`, `set_active_pane_records_last_active_for_its_group`, `activate_pane_direction_never_lands_in_hidden_unit`, `rotate_clockwise_on_tab_with_stack_leaves_invariants_valid` (Review Focus 5; the chosen behaviour is rotate returns without changing anything when `stacks` is non-empty), `swap_panes_and_focus_layout_refuse_with_stack` (they already bail when `hidden` is non-empty: pin that with an assertion on the return value).
- [ ] **Step 2: Run** `... test -p mux --lib lifecycle` (name tests with the `lifecycle_` prefix). Expected: non-zero.
- [ ] **Step 3: Implement** the hooks: call `GroupState::remove_pane` from `remove_pane_if` for every removed pane; when it reports an active-unit removal, `set_unit_hidden(next, false)`. In `set_active_pane`, update the group's `last_active`. Guard `rotate_*` on non-empty `stacks`.
- [ ] **Step 4: Run** the same command. Expected: exit 0.
- [ ] **Step 5: Commit** `Tab: keep groups and stacks consistent on pane removal`.

---

### Task 5: Persistence

**Files:**
- Modify: `mux/src/session_state.rs` (`PaneLayoutNode::Pane`), `mux/src/tab.rs` (`convert_tree_to_session_layout` ~295), `terminaler-gui/src/session.rs` (restore, ~87-190)
- Test: `mux/src/session_state.rs` test module and `mux/src/tab.rs`

**Interfaces:**
- Produces: `PaneLayoutNode::Pane` gains `#[serde(default)] group: Option<u32>` and `#[serde(default)] stack: Option<u32>`. `group` is an ordinal shared by members of one group. `stack` is an ordinal shared by panes of units in one stack. A new `Tab::restore_groups(&self, groups: Vec<Vec<PaneId>>, stacks: Vec<Vec<Vec<PaneId>>>, active_units: Vec<usize>)` rebuilds `GroupState` after panes are re-created.
- Consumes: `GroupState` and `convert_tree_to_session_layout(tree, active, hidden)` (extend its signature with `&GroupState`).

- [ ] **Step 1: Write failing tests**: `old_session_json_without_group_fields_loads_with_no_groups` (Review Focus 4; a JSON literal with only `cwd`, `hidden`, `is_active`), `group_round_trips_through_session_layout`, `stack_with_hidden_unit_round_trips`.
- [ ] **Step 2: Run** `... test -p mux --lib session`. Expected: non-zero.
- [ ] **Step 3: Implement** the fields, the converter and `restore_groups`; in `session.rs`, after the existing `toggle_pane_hidden` restore loop, call `restore_groups` and skip that loop for panes inside a stack (hidden state then comes from `restore_groups`). Invalid saved groups (failing `validate`) are dropped with a `log::warn!`, leaving plain panes.
- [ ] **Step 4: Run** the same command. Expected: exit 0.
- [ ] **Step 5: Commit** `Session: persist pane groups and stacks`.

---

### Task 6: Actions, commands, dispatch and close-group

**Files:**
- Modify: `config/src/keyassignment.rs` (beside `TogglePaneHidden`, ~619), `terminaler-gui/src/commands.rs` (a `CommandDef` near ~1609 and the palette list near ~2241), `terminaler-gui/src/termwindow/mod.rs` (dispatch near ~4708 and the `CloseCurrentPane` handler)

**Interfaces:**
- Produces `KeyAssignment::PaneGroup(PaneDirection)`, `PaneUngroup`, `PaneStackJoin(PaneDirection)`, `PaneStackCycle(PaneDirection)`. Dispatch calls the matching `Tab` method (Task 3), then `self.invalidate_tab_sidebar()` and the window repaint call that `TogglePaneHidden` uses, both (see memory: cache clear and repaint are two calls).
- `CloseCurrentPane { confirm }`: when `tab.unit_panes_of(active).len() > 1`, close every member after the same confirm prompt.

- [ ] **Step 1: Write failing test** in `config` (or the existing keyassignment test module): a config JSON `{ "key": "g", "mods": "CTRL|SHIFT", "action": { "PaneGroup": "Right" } }` parses to `KeyAssignment::PaneGroup(PaneDirection::Right)`; `{ "PaneUngroup": null }` parses likewise.
- [ ] **Step 2: Run** `~/.cargo/bin/cargo test -p config --lib keyassignment > $TMPDIR/t.log 2>&1; echo $?`. Expected: non-zero.
- [ ] **Step 3: Implement** the variants, `CommandDef`s (no `keys`, `menubar: &["Window"]`, `args: &[ArgType::ActivePane]`), dispatch and close-group. Do not add the new variants to any default key table.
- [ ] **Step 4: Run** the config test (exit 0), then `~/.cargo/bin/cargo build > $TMPDIR/b.log 2>&1; echo $?` (exit 0) and the full mux tests `... test -p mux > $TMPDIR/t.log 2>&1; echo $?` (exit 0).
- [ ] **Step 5: Commit** `Actions: PaneGroup, PaneUngroup, PaneStackJoin, PaneStackCycle`.

---

### Task 7: Headless and visual verification

**Files:** none (verification only); record results in `CHANGELOG.md`.

- [ ] **Step 1:** On devbox start `./target/debug/terminaler-mux-server &`, then with `TERMINALER_UNIX_SOCKET` set, spawn three panes via `terminaler cli spawn`. A headless box has no way to invoke the GUI actions, so this step checks only that the build and mux still run and `cli list` shows three panes. State this limit in the report; it does not prove the new actions.
- [ ] **Step 2:** Copy a release build to homepc per the install contract (`~/bin` plus the `.desktop` Exec, `~/.cargo/bin/cargo` on the non-interactive PATH; check children before restarting, see memory). Bind keys in homepc's `terminaler.json` for the four actions (JSONC, snake_case, no trailing commas).
- [ ] **Step 3:** Vilius checks on homepc: group a Claude pane with a shell, stack two such pairs, cycle, restart the GUI and confirm restore, and confirm no ghost/fade glitch on a swap (Open risk 3: unverified until seen).
- [ ] **Step 4:** Add a `CHANGELOG.md` entry, run `cargo clean` on devbox and homepc and delete `~/bin/*.bak-*` (standing wrap-up rule), then commit `Changelog: pane groups`.

---

## Self-review notes

- **Spec coverage:** model and invariants (Task 1), tree handling and stack parent (Task 2), four actions plus palette (Tasks 3, 6), focus and navigation (Task 4), lifecycle and close-group (Tasks 4, 6), persistence (Task 5), testing and phasing (Tasks 1-5, 7). Sidebar is deferred to its own plan, as the spec's phase 2 says.
- **Known unknown:** Task 2's subtree replacement may need more than `remove_pane` plus a new node if `bintree`'s cursor API fights it; if so, fall back to rebuilding the tree as `focus_layout` does (`tab.rs` ~1951) and re-run Task 2's tests.
- **Known limit:** `swap_panes` and `focus_layout` already refuse when any pane is hidden, so they do nothing in a tab with a stack. This is accepted behaviour, pinned by a test in Task 4.
