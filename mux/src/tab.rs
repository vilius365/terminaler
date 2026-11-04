use crate::domain::DomainId;
use crate::pane::*;
use crate::pane_group::{GroupState, PaneStack, PaneUnitInfo, Unit};
use crate::renderable::StableCursorPosition;
use crate::session_state::{layout_leaves, PaneLayoutNode, SplitDirectionState};
use crate::{Mux, MuxNotification, WindowId};
use bintree::PathBranch;
use config::configuration;
use config::keyassignment::PaneDirection;
use parking_lot::Mutex;
use rangeset::intersects_range;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::sync::Arc;
use url::Url;
use terminaler_term::{StableRowIndex, TerminalSize};

pub type Tree = bintree::Tree<Arc<dyn Pane>, SplitDirectionAndSize>;
pub type Cursor = bintree::Cursor<Arc<dyn Pane>, SplitDirectionAndSize>;

static TAB_ID: ::std::sync::atomic::AtomicUsize = ::std::sync::atomic::AtomicUsize::new(0);
pub type TabId = usize;

#[derive(Default)]
struct Recency {
    count: usize,
    by_idx: HashMap<usize, usize>,
}

impl Recency {
    fn tag(&mut self, idx: usize) {
        self.by_idx.insert(idx, self.count);
        self.count += 1;
    }

    fn score(&self, idx: usize) -> usize {
        self.by_idx.get(&idx).copied().unwrap_or(0)
    }
}

struct TabInner {
    id: TabId,
    pane: Option<Tree>,
    size: TerminalSize,
    size_before_zoom: TerminalSize,
    active: usize,
    zoomed: Option<Arc<dyn Pane>>,
    title: String,
    recency: Recency,
    /// Set of pane IDs that are hidden from the layout.
    /// Hidden panes remain alive but don't take up visual space;
    /// their sibling expands to fill the parent allocation.
    hidden: HashSet<PaneId>,
    /// Set by focus_layout and cleared when a split adds a pane, so a
    /// resize is only read as a focus width while the tree is one that
    /// focus_layout built.
    focus_arranged: bool,
    /// Pane groups and stacks laid over the split tree.
    groups: GroupState,
    /// Set while dock_unit moves panes, so removing them does not prune
    /// the groups they belong to.
    docking: bool,
}

/// A Tab is a container of Panes
pub struct Tab {
    inner: Mutex<TabInner>,
    tab_id: TabId,
}

#[derive(Clone)]
pub struct PositionedPane {
    /// The topological pane index that can be used to reference this pane
    pub index: usize,
    /// true if this is the active pane at the time the position was computed
    pub is_active: bool,
    /// true if this pane is zoomed
    pub is_zoomed: bool,
    /// The offset from the top left corner of the containing tab to the top
    /// left corner of this pane, in cells.
    pub left: usize,
    /// The offset from the top left corner of the containing tab to the top
    /// left corner of this pane, in cells.
    pub top: usize,
    /// The width of this pane in cells
    pub width: usize,
    pub pixel_width: usize,
    /// The height of this pane in cells
    pub height: usize,
    pub pixel_height: usize,
    /// The pane instance
    pub pane: Arc<dyn Pane>,
}

impl std::fmt::Debug for PositionedPane {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        fmt.debug_struct("PositionedPane")
            .field("index", &self.index)
            .field("is_active", &self.is_active)
            .field("left", &self.left)
            .field("top", &self.top)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pane_id", &self.pane.pane_id())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

/// The size is of the (first, second) child of the split
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct SplitDirectionAndSize {
    pub direction: SplitDirection,
    pub first: TerminalSize,
    pub second: TerminalSize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum SplitSize {
    Cells(usize),
    Percent(u8),
}

impl Default for SplitSize {
    fn default() -> Self {
        Self::Percent(50)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct SplitRequest {
    pub direction: SplitDirection,
    /// Whether the newly created item will be in the second part
    /// of the split (right/bottom)
    pub target_is_second: bool,
    /// Split across the top of the tab rather than the active pane
    pub top_level: bool,
    /// The size of the new item
    pub size: SplitSize,
}

impl Default for SplitRequest {
    fn default() -> Self {
        Self {
            direction: SplitDirection::Horizontal,
            target_is_second: true,
            top_level: false,
            size: SplitSize::default(),
        }
    }
}

impl SplitDirectionAndSize {
    fn top_of_second(&self) -> usize {
        match self.direction {
            SplitDirection::Horizontal => 0,
            SplitDirection::Vertical => self.first.rows as usize + 1,
        }
    }

    fn left_of_second(&self) -> usize {
        match self.direction {
            SplitDirection::Horizontal => self.first.cols as usize + 1,
            SplitDirection::Vertical => 0,
        }
    }

    pub fn width(&self) -> usize {
        if self.direction == SplitDirection::Horizontal {
            self.first.cols + self.second.cols + 1
        } else {
            self.first.cols
        }
    }

    pub fn height(&self) -> usize {
        if self.direction == SplitDirection::Vertical {
            self.first.rows + self.second.rows + 1
        } else {
            self.first.rows
        }
    }

    pub fn size(&self) -> TerminalSize {
        let cell_width = self.first.pixel_width / self.first.cols;
        let cell_height = self.first.pixel_height / self.first.rows;

        let rows = self.height();
        let cols = self.width();

        TerminalSize {
            rows,
            cols,
            pixel_height: cell_height * rows,
            pixel_width: cell_width * cols,
            dpi: self.first.dpi,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PositionedSplit {
    /// The topological node index that can be used to reference this split
    pub index: usize,
    pub direction: SplitDirection,
    /// The offset from the top left corner of the containing tab to the top
    /// left corner of this split, in cells.
    pub left: usize,
    /// The offset from the top left corner of the containing tab to the top
    /// left corner of this split, in cells.
    pub top: usize,
    /// For Horizontal splits, how tall the split should be, for Vertical
    /// splits how wide it should be
    pub size: usize,
}

fn is_pane(pane: &Arc<dyn Pane>, other: &Option<&Arc<dyn Pane>>) -> bool {
    if let Some(other) = other {
        other.pane_id() == pane.pane_id()
    } else {
        false
    }
}

fn pane_tree(
    tree: &Tree,
    tab_id: TabId,
    window_id: WindowId,
    active: Option<&Arc<dyn Pane>>,
    zoomed: Option<&Arc<dyn Pane>>,
    workspace: &str,
    left_col: usize,
    top_row: usize,
) -> PaneNode {
    match tree {
        Tree::Empty => PaneNode::Empty,
        Tree::Node { left, right, data } => {
            let data = data.unwrap();
            PaneNode::Split {
                left: Box::new(pane_tree(
                    &*left, tab_id, window_id, active, zoomed, workspace, left_col, top_row,
                )),
                right: Box::new(pane_tree(
                    &*right,
                    tab_id,
                    window_id,
                    active,
                    zoomed,
                    workspace,
                    if data.direction == SplitDirection::Vertical {
                        left_col
                    } else {
                        left_col + data.left_of_second()
                    },
                    if data.direction == SplitDirection::Horizontal {
                        top_row
                    } else {
                        top_row + data.top_of_second()
                    },
                )),
                node: data,
            }
        }
        Tree::Leaf(pane) => {
            let dims = pane.get_dimensions();
            let working_dir = pane.get_current_working_dir(CachePolicy::AllowStale);
            let cursor_pos = pane.get_cursor_position();

            PaneNode::Leaf(PaneEntry {
                window_id,
                tab_id,
                pane_id: pane.pane_id(),
                title: pane.get_title(),
                is_active_pane: is_pane(pane, &active),
                is_zoomed_pane: is_pane(pane, &zoomed),
                size: TerminalSize {
                    cols: dims.cols,
                    rows: dims.viewport_rows,
                    pixel_height: dims.pixel_height,
                    pixel_width: dims.pixel_width,
                    dpi: dims.dpi,
                },
                working_dir: working_dir.map(Into::into),
                workspace: workspace.to_string(),
                cursor_pos,
                physical_top: dims.physical_top,
                left_col,
                top_row,
                tty_name: pane.tty_name(),
            })
        }
    }
}

/// Convert a bintree into a PaneLayoutNode for session persistence.
/// Captures actual split directions and size ratios from the tree.
fn convert_tree_to_session_layout(
    tree: &Tree,
    active: Option<&Arc<dyn Pane>>,
    hidden: &HashSet<PaneId>,
    groups: &GroupState,
) -> PaneLayoutNode {
    match tree {
        Tree::Empty => PaneLayoutNode::Pane {
            cwd: None,
            command: None,
            hidden: false,
            is_active: false,
            group: None,
            stack: None,
            stack_pos: None,
        },
        Tree::Leaf(pane) => {
            let cwd = pane
                .get_current_working_dir(CachePolicy::AllowStale)
                .and_then(|u| u.to_file_path().ok())
                .map(|p| p.into());
            let is_active = active.map_or(false, |a| a.pane_id() == pane.pane_id());
            let unit = groups.unit_of(pane.pane_id());
            let (stack, stack_pos) = match groups.stack_of(&unit) {
                Some(index) => (
                    Some(index as u32),
                    groups.stacks[index]
                        .units
                        .iter()
                        .position(|u| *u == unit)
                        .map(|pos| pos as u32),
                ),
                None => (None, None),
            };
            PaneLayoutNode::Pane {
                cwd,
                command: None,
                hidden: hidden.contains(&pane.pane_id()),
                is_active,
                group: match unit {
                    Unit::Group(id) => Some(id),
                    Unit::Pane(_) => None,
                },
                stack,
                stack_pos,
            }
        }
        Tree::Node { left, right, data } => {
            let data = data.unwrap();
            let direction = match data.direction {
                SplitDirection::Horizontal => SplitDirectionState::Horizontal,
                SplitDirection::Vertical => SplitDirectionState::Vertical,
            };
            let ratio = match data.direction {
                SplitDirection::Horizontal => {
                    let total = data.first.cols as f64 + data.second.cols as f64;
                    if total > 0.0 {
                        data.first.cols as f64 / total
                    } else {
                        0.5
                    }
                }
                SplitDirection::Vertical => {
                    let total = data.first.rows as f64 + data.second.rows as f64;
                    if total > 0.0 {
                        data.first.rows as f64 / total
                    } else {
                        0.5
                    }
                }
            };
            PaneLayoutNode::Split {
                direction,
                ratio,
                first: Box::new(convert_tree_to_session_layout(left, active, hidden, groups)),
                second: Box::new(convert_tree_to_session_layout(right, active, hidden, groups)),
            }
        }
    }
}

fn build_from_pane_tree<F>(
    tree: bintree::Tree<PaneEntry, SplitDirectionAndSize>,
    active: &mut Option<Arc<dyn Pane>>,
    zoomed: &mut Option<Arc<dyn Pane>>,
    make_pane: &mut F,
) -> Tree
where
    F: FnMut(PaneEntry) -> Arc<dyn Pane>,
{
    match tree {
        bintree::Tree::Empty => Tree::Empty,
        bintree::Tree::Node { left, right, data } => Tree::Node {
            left: Box::new(build_from_pane_tree(*left, active, zoomed, make_pane)),
            right: Box::new(build_from_pane_tree(*right, active, zoomed, make_pane)),
            data,
        },
        bintree::Tree::Leaf(entry) => {
            let is_zoomed_pane = entry.is_zoomed_pane;
            let is_active_pane = entry.is_active_pane;
            let pane = make_pane(entry);
            if is_zoomed_pane {
                zoomed.replace(Arc::clone(&pane));
            }
            if is_active_pane {
                active.replace(Arc::clone(&pane));
            }
            Tree::Leaf(pane)
        }
    }
}

/// Computes the minimum (x, y) size based on the panes in this portion
/// of the tree.
fn compute_min_size(tree: &mut Tree) -> (usize, usize) {
    match tree {
        Tree::Node { data: None, .. } | Tree::Empty => (1, 1),
        Tree::Node {
            left,
            right,
            data: Some(data),
        } => {
            let (left_x, left_y) = compute_min_size(&mut *left);
            let (right_x, right_y) = compute_min_size(&mut *right);
            match data.direction {
                SplitDirection::Vertical => (left_x.max(right_x), left_y + right_y + 1),
                SplitDirection::Horizontal => (left_x + right_x + 1, left_y.max(right_y)),
            }
        }
        Tree::Leaf(_) => (1, 1),
    }
}

fn adjust_x_size(tree: &mut Tree, mut x_adjust: isize, cell_dimensions: &TerminalSize) {
    let (min_x, _) = compute_min_size(tree);
    while x_adjust != 0 {
        match tree {
            Tree::Empty | Tree::Leaf(_) => return,
            Tree::Node { data: None, .. } => return,
            Tree::Node {
                left,
                right,
                data: Some(data),
            } => {
                data.first.dpi = cell_dimensions.dpi;
                data.second.dpi = cell_dimensions.dpi;
                match data.direction {
                    SplitDirection::Vertical => {
                        let new_cols = (data.first.cols as isize)
                            .saturating_add(x_adjust)
                            .max(min_x as isize);
                        x_adjust = new_cols.saturating_sub(data.first.cols as isize);

                        if x_adjust != 0 {
                            adjust_x_size(&mut *left, x_adjust, cell_dimensions);
                            data.first.cols = new_cols.try_into().unwrap();
                            data.first.pixel_width =
                                data.first.cols.saturating_mul(cell_dimensions.pixel_width);

                            adjust_x_size(&mut *right, x_adjust, cell_dimensions);
                            data.second.cols = data.first.cols;
                            data.second.pixel_width = data.first.pixel_width;
                        }
                        return;
                    }
                    SplitDirection::Horizontal if x_adjust > 0 => {
                        adjust_x_size(&mut *left, 1, cell_dimensions);
                        data.first.cols += 1;
                        data.first.pixel_width =
                            data.first.cols.saturating_mul(cell_dimensions.pixel_width);
                        x_adjust -= 1;

                        if x_adjust > 0 {
                            adjust_x_size(&mut *right, 1, cell_dimensions);
                            data.second.cols += 1;
                            data.second.pixel_width =
                                data.second.cols.saturating_mul(cell_dimensions.pixel_width);
                            x_adjust -= 1;
                        }
                    }
                    SplitDirection::Horizontal => {
                        // x_adjust is negative
                        if data.first.cols > 1 {
                            adjust_x_size(&mut *left, -1, cell_dimensions);
                            data.first.cols -= 1;
                            data.first.pixel_width =
                                data.first.cols.saturating_mul(cell_dimensions.pixel_width);
                            x_adjust += 1;
                        }
                        if x_adjust < 0 && data.second.cols > 1 {
                            adjust_x_size(&mut *right, -1, cell_dimensions);
                            data.second.cols -= 1;
                            data.second.pixel_width =
                                data.second.cols.saturating_mul(cell_dimensions.pixel_width);
                            x_adjust += 1;
                        }
                    }
                }
            }
        }
    }
}

fn adjust_y_size(tree: &mut Tree, mut y_adjust: isize, cell_dimensions: &TerminalSize) {
    let (_, min_y) = compute_min_size(tree);
    while y_adjust != 0 {
        match tree {
            Tree::Empty | Tree::Leaf(_) => return,
            Tree::Node { data: None, .. } => return,
            Tree::Node {
                left,
                right,
                data: Some(data),
            } => {
                data.first.dpi = cell_dimensions.dpi;
                data.second.dpi = cell_dimensions.dpi;
                match data.direction {
                    SplitDirection::Horizontal => {
                        let new_rows = (data.first.rows as isize)
                            .saturating_add(y_adjust)
                            .max(min_y as isize);
                        y_adjust = new_rows.saturating_sub(data.first.rows as isize);

                        if y_adjust != 0 {
                            adjust_y_size(&mut *left, y_adjust, cell_dimensions);
                            data.first.rows = new_rows.try_into().unwrap();
                            data.first.pixel_height =
                                data.first.rows.saturating_mul(cell_dimensions.pixel_height);

                            adjust_y_size(&mut *right, y_adjust, cell_dimensions);
                            data.second.rows = data.first.rows;
                            data.second.pixel_height = data.first.pixel_height;
                        }
                        return;
                    }
                    SplitDirection::Vertical if y_adjust > 0 => {
                        adjust_y_size(&mut *left, 1, cell_dimensions);
                        data.first.rows += 1;
                        data.first.pixel_height =
                            data.first.rows.saturating_mul(cell_dimensions.pixel_height);
                        y_adjust -= 1;
                        if y_adjust > 0 {
                            adjust_y_size(&mut *right, 1, cell_dimensions);
                            data.second.rows += 1;
                            data.second.pixel_height = data
                                .second
                                .rows
                                .saturating_mul(cell_dimensions.pixel_height);
                            y_adjust -= 1;
                        }
                    }
                    SplitDirection::Vertical => {
                        // y_adjust is negative
                        if data.first.rows > 1 {
                            adjust_y_size(&mut *left, -1, cell_dimensions);
                            data.first.rows -= 1;
                            data.first.pixel_height =
                                data.first.rows.saturating_mul(cell_dimensions.pixel_height);
                            y_adjust += 1;
                        }
                        if y_adjust < 0 && data.second.rows > 1 {
                            adjust_y_size(&mut *right, -1, cell_dimensions);
                            data.second.rows -= 1;
                            data.second.pixel_height = data
                                .second
                                .rows
                                .saturating_mul(cell_dimensions.pixel_height);
                            y_adjust += 1;
                        }
                    }
                }
            }
        }
    }
}

fn apply_sizes_from_splits(tree: &Tree, size: &TerminalSize) {
    apply_sizes_from_splits_with_hidden(tree, size, &HashSet::new())
}

/// Apply sizes from the split tree, accounting for hidden panes.
/// When one side of a split is entirely hidden, the visible side
/// gets the full parent allocation.
fn apply_sizes_from_splits_with_hidden(
    tree: &Tree,
    size: &TerminalSize,
    hidden: &HashSet<PaneId>,
) {
    match tree {
        Tree::Empty => return,
        Tree::Node { data: None, .. } => return,
        Tree::Node {
            left,
            right,
            data: Some(data),
        } => {
            let left_hidden = is_subtree_hidden(left, hidden);
            let right_hidden = is_subtree_hidden(right, hidden);

            if left_hidden && !right_hidden {
                // Left is hidden: right gets full parent size
                apply_sizes_from_splits_with_hidden(right, size, hidden);
            } else if right_hidden && !left_hidden {
                // Right is hidden: left gets full parent size
                apply_sizes_from_splits_with_hidden(left, size, hidden);
            } else {
                // Both visible (or both hidden) — use normal split sizes
                apply_sizes_from_splits_with_hidden(left, &data.first, hidden);
                apply_sizes_from_splits_with_hidden(right, &data.second, hidden);
            }
        }
        Tree::Leaf(pane) => {
            if !hidden.contains(&pane.pane_id()) {
                pane.resize(*size).ok();
            }
        }
    }
}

/// Recursively collect visible (non-hidden) panes with correct positions,
/// giving hidden panes' space to their siblings.
fn collect_visible_panes(
    tree: &Tree,
    size: &TerminalSize,
    left: usize,
    top: usize,
    active_idx: usize,
    zoomed_id: Option<PaneId>,
    hidden: &HashSet<PaneId>,
    order: &[PaneId],
    panes: &mut Vec<PositionedPane>,
) {
    match tree {
        Tree::Empty => {}
        Tree::Leaf(pane) => {
            if !hidden.contains(&pane.pane_id()) {
                // Indices count hidden panes too, so they agree with
                // `TabInner::active` and `set_active_idx`.
                let index = order
                    .iter()
                    .position(|id| *id == pane.pane_id())
                    .unwrap_or(panes.len());
                panes.push(PositionedPane {
                    index,
                    is_active: index == active_idx,
                    is_zoomed: zoomed_id == Some(pane.pane_id()),
                    left,
                    top,
                    width: size.cols as _,
                    height: size.rows as _,
                    pixel_width: size.pixel_width as _,
                    pixel_height: size.pixel_height as _,
                    pane: Arc::clone(pane),
                });
            }
        }
        Tree::Node {
            left: left_tree,
            right: right_tree,
            data: Some(data),
        } => {
            let left_hidden = is_subtree_hidden(left_tree, hidden);
            let right_hidden = is_subtree_hidden(right_tree, hidden);

            if left_hidden && right_hidden {
                // Both sides hidden — nothing to render
            } else if left_hidden {
                // Left hidden: right gets full parent size at parent position
                collect_visible_panes(
                    right_tree, size, left, top,
                    active_idx, zoomed_id, hidden, order, panes,
                );
            } else if right_hidden {
                // Right hidden: left gets full parent size at parent position
                collect_visible_panes(
                    left_tree, size, left, top,
                    active_idx, zoomed_id, hidden, order, panes,
                );
            } else {
                // Both visible — use normal split sizes
                collect_visible_panes(
                    left_tree, &data.first, left, top,
                    active_idx, zoomed_id, hidden, order, panes,
                );
                // One cell of divider sits between the halves, as in
                // `iter_panes_impl`.
                let (right_left, right_top) = match data.direction {
                    SplitDirection::Horizontal => (left + data.first.cols as usize + 1, top),
                    SplitDirection::Vertical => (left, top + data.first.rows as usize + 1),
                };
                collect_visible_panes(
                    right_tree, &data.second, right_left, right_top,
                    active_idx, zoomed_id, hidden, order, panes,
                );
            }
        }
        Tree::Node { data: None, .. } => {}
    }
}

/// Recursively collect visible splits, skipping splits where either side is entirely hidden.
fn collect_visible_splits(
    tree: &Tree,
    left: usize,
    top: usize,
    hidden: &HashSet<PaneId>,
    dividers: &mut Vec<PositionedSplit>,
    index: &mut usize,
) {
    match tree {
        Tree::Empty | Tree::Leaf(_) => {}
        Tree::Node {
            left: left_tree,
            right: right_tree,
            data: Some(data),
        } => {
            let left_hidden = is_subtree_hidden(left_tree, hidden);
            let right_hidden = is_subtree_hidden(right_tree, hidden);

            if left_hidden && right_hidden {
                // Both hidden — no splits to show
            } else if left_hidden {
                // Left hidden — skip this split, recurse into right with parent position
                collect_visible_splits(right_tree, left, top, hidden, dividers, index);
            } else if right_hidden {
                // Right hidden — skip this split, recurse into left with parent position
                collect_visible_splits(left_tree, left, top, hidden, dividers, index);
            } else {
                // Both visible — emit split divider and recurse normally
                let split_left = left + match data.direction {
                    SplitDirection::Horizontal => data.first.cols as usize,
                    SplitDirection::Vertical => 0,
                };
                let split_top = top + match data.direction {
                    SplitDirection::Horizontal => 0,
                    SplitDirection::Vertical => data.first.rows as usize,
                };

                dividers.push(PositionedSplit {
                    index: *index,
                    direction: data.direction,
                    left: split_left,
                    top: split_top,
                    size: if data.direction == SplitDirection::Horizontal {
                        data.height() as usize
                    } else {
                        data.width() as usize
                    },
                });
                *index += 1;

                collect_visible_splits(left_tree, left, top, hidden, dividers, index);
                let (right_left, right_top) = match data.direction {
                    SplitDirection::Horizontal => (left + data.first.cols as usize, top),
                    SplitDirection::Vertical => (left, top + data.first.rows as usize),
                };
                collect_visible_splits(right_tree, right_left, right_top, hidden, dividers, index);
            }
        }
        Tree::Node { data: None, .. } => {}
    }
}

/// Returns true if every leaf pane in the subtree is hidden.
fn is_subtree_hidden(tree: &Tree, hidden: &HashSet<PaneId>) -> bool {
    match tree {
        Tree::Empty => true,
        Tree::Leaf(pane) => hidden.contains(&pane.pane_id()),
        Tree::Node { left, right, .. } => {
            is_subtree_hidden(left, hidden) && is_subtree_hidden(right, hidden)
        }
    }
}

fn split_dimension(dim: usize, request: SplitRequest) -> (usize, usize) {
    let target_size = match request.size {
        SplitSize::Cells(n) => n,
        SplitSize::Percent(n) => (dim * (n as usize)) / 100,
    }
    .max(1);

    let remain = dim.saturating_sub(target_size + 1);

    if request.target_is_second {
        (remain, target_size)
    } else {
        (target_size, remain)
    }
}

/// The split that divides a `width` x `height` region (in cells) as
/// `request` describes.
fn split_size_for_extent(
    cell_dims: &TerminalSize,
    width: usize,
    height: usize,
    request: SplitRequest,
) -> SplitDirectionAndSize {
    let ((width1, width2), (height1, height2)) = match request.direction {
        SplitDirection::Horizontal => (split_dimension(width, request), (height, height)),
        SplitDirection::Vertical => ((width, width), split_dimension(height, request)),
    };

    SplitDirectionAndSize {
        direction: request.direction,
        first: TerminalSize {
            rows: height1 as _,
            cols: width1 as _,
            pixel_height: cell_dims.pixel_height * height1,
            pixel_width: cell_dims.pixel_width * width1,
            dpi: cell_dims.dpi,
        },
        second: TerminalSize {
            rows: height2 as _,
            cols: width2 as _,
            pixel_height: cell_dims.pixel_height * height2,
            pixel_width: cell_dims.pixel_width * width2,
            dpi: cell_dims.dpi,
        },
    }
}

/// The leaf pane ids of `tree` in topological order.
fn leaf_order(tree: &Tree) -> Vec<PaneId> {
    match tree {
        Tree::Empty => vec![],
        Tree::Leaf(pane) => vec![pane.pane_id()],
        Tree::Node { left, right, .. } => {
            let mut ids = leaf_order(left);
            ids.extend(leaf_order(right));
            ids
        }
    }
}

/// The leaf pane ids of `tree`.
fn leaf_ids(tree: &Tree) -> HashSet<PaneId> {
    match tree {
        Tree::Empty => HashSet::new(),
        Tree::Leaf(pane) => std::iter::once(pane.pane_id()).collect(),
        Tree::Node { left, right, .. } => {
            let mut ids = leaf_ids(left);
            ids.extend(leaf_ids(right));
            ids
        }
    }
}

/// The leaf set of every subtree of `tree`, leaves included.
pub(crate) fn subtree_leaf_sets(tree: &Tree) -> Vec<HashSet<PaneId>> {
    fn walk(tree: &Tree, out: &mut Vec<HashSet<PaneId>>) -> HashSet<PaneId> {
        let ids = match tree {
            Tree::Empty => return HashSet::new(),
            Tree::Leaf(pane) => std::iter::once(pane.pane_id()).collect(),
            Tree::Node { left, right, .. } => {
                let mut ids = walk(left, out);
                ids.extend(walk(right, out));
                ids
            }
        };
        out.push(ids.clone());
        ids
    }
    let mut out = vec![];
    walk(tree, &mut out);
    out
}

/// Replaces the subtree whose leaves are exactly `target` with a split node
/// holding that subtree and `docked`. Returns whether it found the subtree.
fn replace_subtree_with_pair(
    tree: Tree,
    target: &HashSet<PaneId>,
    docked: &mut Option<Tree>,
    data: &SplitDirectionAndSize,
    docked_second: bool,
) -> (Tree, bool) {
    if &leaf_ids(&tree) == target {
        let Some(new) = docked.take() else {
            return (tree, false);
        };
        let (left, right) = if docked_second { (tree, new) } else { (new, tree) };
        return (
            Tree::Node {
                left: Box::new(left),
                right: Box::new(right),
                data: Some(data.clone()),
            },
            true,
        );
    }
    match tree {
        Tree::Node { left, right, data: node_data } => {
            let (left, found) = replace_subtree_with_pair(*left, target, docked, data, docked_second);
            if found {
                return (
                    Tree::Node { left: Box::new(left), right, data: node_data },
                    true,
                );
            }
            let (right, found) = replace_subtree_with_pair(*right, target, docked, data, docked_second);
            (
                Tree::Node { left: Box::new(left), right: Box::new(right), data: node_data },
                found,
            )
        }
        other => (other, false),
    }
}

/// `panes` chained left to right as a tree, with each pane given an equal
/// share of `width` x `height`. Needs at least one pane.
fn chain_subtree(
    panes: &[Arc<dyn Pane>],
    width: usize,
    height: usize,
    cell_dims: &TerminalSize,
) -> Tree {
    let [first, rest @ ..] = panes else {
        return Tree::Empty;
    };
    if rest.is_empty() {
        return Tree::Leaf(Arc::clone(first));
    }
    let request = SplitRequest {
        direction: SplitDirection::Horizontal,
        target_is_second: false,
        top_level: false,
        size: SplitSize::Cells((width.saturating_sub(rest.len()) / panes.len()).max(1)),
    };
    let data = split_size_for_extent(cell_dims, width, height, request);
    let rest_width = data.second.cols;
    Tree::Node {
        left: Box::new(Tree::Leaf(Arc::clone(first))),
        right: Box::new(chain_subtree(rest, rest_width, height, cell_dims)),
        data: Some(data),
    }
}

/// The split that docks a unit on the side of its neighbour opposite
/// `direction`: with the neighbour to the right, the unit lands on its left.
fn dock_request(direction: PaneDirection, percent: u8) -> SplitRequest {
    SplitRequest {
        direction: match direction {
            PaneDirection::Left | PaneDirection::Right => SplitDirection::Horizontal,
            _ => SplitDirection::Vertical,
        },
        target_is_second: matches!(direction, PaneDirection::Left | PaneDirection::Up),
        top_level: false,
        size: SplitSize::Percent(percent),
    }
}

fn cell_dimensions(size: &TerminalSize) -> TerminalSize {
    TerminalSize {
        rows: 1,
        cols: 1,
        pixel_width: size.pixel_width / size.cols,
        pixel_height: size.pixel_height / size.rows,
        dpi: size.dpi,
    }
}

impl Tab {
    pub fn new(size: &TerminalSize) -> Self {
        let inner = TabInner::new(size);
        let tab_id = inner.id;
        Self {
            inner: Mutex::new(inner),
            tab_id,
        }
    }

    pub fn get_title(&self) -> String {
        self.inner.lock().title.clone()
    }

    pub fn set_title(&self, title: &str) {
        let mut inner = self.inner.lock();
        if inner.title != title {
            inner.title = title.to_string();
            Mux::try_get().map(|mux| {
                mux.notify(MuxNotification::TabTitleChanged {
                    tab_id: inner.id,
                    title: title.to_string(),
                })
            });
        }
    }

    /// Called by the multiplexer client when building a local tab to
    /// mirror a remote tab.  The supplied `root` is the information
    /// about our counterpart in the the remote server.
    /// This method builds a local tree based on the remote tree which
    /// then replaces the local tree structure.
    ///
    /// The `make_pane` function is provided by the caller, and its purpose
    /// is to lookup an existing Pane that corresponds to the provided
    /// PaneEntry, or to create a new Pane from that entry.
    /// make_pane is expected to add the pane to the mux if it creates
    /// a new pane, otherwise the pane won't poll/update in the GUI.
    pub fn sync_with_pane_tree<F>(&self, size: TerminalSize, root: PaneNode, make_pane: F)
    where
        F: FnMut(PaneEntry) -> Arc<dyn Pane>,
    {
        self.inner.lock().sync_with_pane_tree(size, root, make_pane)
    }

    pub fn codec_pane_tree(&self) -> PaneNode {
        self.inner.lock().codec_pane_tree()
    }

    /// Returns the pane layout tree for session persistence.
    /// Unlike codec_pane_tree, this produces a PaneLayoutNode that captures
    /// actual split directions and size ratios from the bintree.
    pub fn session_layout_tree(&self) -> PaneLayoutNode {
        self.inner.lock().session_layout_tree()
    }

    /// Returns a count of how many panes are in this tab
    pub fn count_panes(&self) -> Option<usize> {
        self.inner.try_lock().map(|mut inner| inner.count_panes())
    }

    /// Sets the zoom state, returns the prior state
    pub fn set_zoomed(&self, zoomed: bool) -> bool {
        self.inner.lock().set_zoomed(zoomed)
    }

    pub fn toggle_zoom(&self) {
        self.inner.lock().toggle_zoom()
    }

    /// Toggle the hidden state of a pane.
    /// Hidden panes remain alive but are removed from the visual layout;
    /// their sibling expands to fill the space.
    pub fn toggle_pane_hidden(&self, pane_id: PaneId) {
        self.inner.lock().toggle_pane_hidden(pane_id)
    }

    /// Returns true if the given pane is hidden.
    pub fn is_pane_hidden(&self, pane_id: PaneId) -> bool {
        self.inner.lock().hidden.contains(&pane_id)
    }

    /// Returns the set of currently hidden pane IDs.
    pub fn hidden_pane_ids(&self) -> HashSet<PaneId> {
        self.inner.lock().hidden.clone()
    }

    pub fn contains_pane(&self, pane: PaneId) -> bool {
        self.inner.lock().contains_pane(pane)
    }

    pub fn iter_panes(&self) -> Vec<PositionedPane> {
        self.inner.lock().iter_panes()
    }

    pub fn iter_panes_ignoring_zoom(&self) -> Vec<PositionedPane> {
        self.inner.lock().iter_panes_ignoring_zoom()
    }

    pub fn rotate_counter_clockwise(&self) {
        self.inner.lock().rotate_counter_clockwise()
    }

    pub fn rotate_clockwise(&self) {
        self.inner.lock().rotate_clockwise()
    }

    pub fn iter_splits(&self) -> Vec<PositionedSplit> {
        self.inner.lock().iter_splits()
    }

    pub fn tab_id(&self) -> TabId {
        self.tab_id
    }

    pub fn get_size(&self) -> TerminalSize {
        self.inner.lock().get_size()
    }

    /// Apply the new size of the tab to the panes contained within.
    /// The delta between the current and the new size is computed,
    /// and is distributed between the splits.  For small resizes
    /// this algorithm biases towards adjusting the left/top nodes
    /// first.  For large resizes this tends to proportionally adjust
    /// the relative sizes of the elements in a split.
    pub fn resize(&self, size: TerminalSize) {
        self.inner.lock().resize(size)
    }

    /// Called when running in the mux server after an individual pane
    /// has been resized.
    /// Because the split manipulation happened on the GUI we "lost"
    /// the information that would have allowed us to call resize_split_by()
    /// and instead need to back-infer the split size information.
    /// We rely on the client to have resized (or be in the process
    /// of resizing) affected panes consistently with its own Tab
    /// tree model.
    /// This method does a simple tree walk to the leaves to back-propagate
    /// the size of the panes up to their containing node split data.
    /// Without this step, disconnecting and reconnecting would cause
    /// the GUI to use stale size information for the window it spawns
    /// to attach this tab.
    pub fn rebuild_splits_sizes_from_contained_panes(&self) {
        self.inner
            .lock()
            .rebuild_splits_sizes_from_contained_panes()
    }

    /// Given split_index, the topological index of a split returned by
    /// iter_splits() as PositionedSplit::index, revised the split position
    /// by the provided delta; positive values move the split to the right/bottom,
    /// and negative values to the left/top.
    /// The adjusted size is propogated downwards to contained children and
    /// their panes are resized accordingly.
    pub fn resize_split_by(&self, split_index: usize, delta: isize) {
        let mut inner = self.inner.lock();
        inner.resize_split_by(split_index, delta);
        inner.remember_focus_width();
    }

    /// Flip the split containing `pane_id` between a left/right and a
    /// top/bottom arrangement. Returns false when the pane is not in a split,
    /// so a caller can leave the control out of its UI.
    pub fn toggle_split_direction_for_pane(&self, pane_id: PaneId) -> bool {
        self.inner.lock().toggle_split_direction_for_pane(pane_id)
    }

    /// Rearrange every pane of the tab around `pane_id`: it takes
    /// `main_fraction` of the width on the left, and all other panes stack
    /// top to bottom on the right in their current order. No pane is created
    /// or closed. Returns false, leaving the layout untouched, when the tab
    /// has hidden panes, fewer than two panes, or too few rows for the stack.
    pub fn focus_layout(&self, pane_id: PaneId, main_fraction: f32) -> bool {
        self.inner.lock().focus_layout(pane_id, main_fraction)
    }

    /// The main pane's share of the width while the tab is still in the
    /// arrangement focus_layout built, or `None` once it is not.
    pub fn focus_main_fraction(&self) -> Option<f32> {
        self.inner.lock().focus_main_fraction()
    }

    /// Trade the places of two panes. Each pane takes over the other's slot
    /// and size, so the shape of the layout does not change. The active pane
    /// stays the same pane. Returns false, leaving the layout untouched, when
    /// the panes are the same or not both in the tab, or the tab is zoomed or
    /// has hidden panes.
    pub fn swap_panes(&self, a: PaneId, b: PaneId) -> bool {
        self.inner.lock().swap_panes(a, b)
    }

    /// Rebuild the groups and stacks a saved `layout` recorded, once the
    /// tab has its panes (in the saved order) and its hidden panes back.
    /// Returns false, leaving the tab with no groups, when the layout does
    /// not describe valid groups for this tab's tree.
    pub fn restore_groups(&self, layout: &PaneLayoutNode) -> bool {
        self.inner.lock().restore_groups(layout)
    }

    /// Bind the active unit and its neighbour in `direction` (Up, Down, Left
    /// or Right) into one group. Returns false, changing nothing, when there
    /// is no neighbour, both are already one unit, either sits in a stack,
    /// or the tab is zoomed.
    pub fn group_panes(&self, direction: PaneDirection) -> bool {
        self.inner.lock().group_panes(direction)
    }

    /// Bind the active unit and the unit holding the visible pane `target`
    /// into one group, wherever `target` sits. Same refusals as
    /// `group_panes`, plus: `target` is the active pane or not visible.
    pub fn group_with(&self, target: PaneId) -> bool {
        self.inner.lock().group_with(target)
    }

    /// Like `stack_join`, for the unit holding the visible pane `target`
    /// instead of a neighbour in a direction.
    pub fn stack_join_with(&self, target: PaneId) -> bool {
        self.inner.lock().stack_join_with(target)
    }

    /// Group and stack membership of every pane in the tab.
    pub fn pane_unit_info(&self) -> HashMap<PaneId, PaneUnitInfo> {
        self.inner.lock().pane_unit_info()
    }

    /// Dissolve the active pane's group back into plain panes. Returns false
    /// when the active pane is in no group or its group sits in a stack.
    pub fn ungroup_active(&self) -> bool {
        self.inner.lock().ungroup_active()
    }

    /// Move the active unit into the slot of its neighbour in `direction`
    /// (Up, Down, Left or Right), making both a stack with the moved unit on
    /// show. Returns false, changing nothing, when there is no neighbour, the
    /// active unit is already stacked, or the tab is zoomed.
    pub fn stack_join(&self, direction: PaneDirection) -> bool {
        self.inner.lock().stack_join(direction)
    }

    /// Show the next (`Next`) or previous (`Prev`) unit of the active unit's
    /// stack. Any other direction, or an active unit in no stack, returns
    /// false.
    pub fn stack_cycle(&self, direction: PaneDirection) -> bool {
        self.inner.lock().stack_cycle(direction)
    }

    /// The panes that move with `pane`: its whole group, or just itself.
    pub fn unit_panes_of(&self, pane: PaneId) -> Vec<PaneId> {
        let inner = self.inner.lock();
        inner.groups.unit_panes(&inner.groups.unit_of(pane))
    }

    /// Move every pane of `source` out of its place and dock them, chained
    /// side by side, against the whole subtree made of exactly the `target`
    /// panes, as the split `request` describes. The panes keep running and
    /// the first of them becomes the active one. Returns false, leaving the
    /// layout untouched, when a pane is missing, `source` is empty or shares
    /// a pane with `target`, the tab is zoomed, or `target` is not one
    /// subtree of the layout.
    pub fn dock_unit(&self, source: &[PaneId], target: &[PaneId], request: SplitRequest) -> bool {
        self.inner.lock().dock_unit(source, target, request)
    }

    /// Move `source` out of its place and dock it against `target` as the
    /// split `request` describes (the same request a drag-to-split builds).
    /// The pane keeps running and becomes the active one. Returns false,
    /// leaving the layout untouched, when the panes are the same or not both
    /// in the tab, the tab is zoomed, or `target` is too small to split.
    pub fn dock_pane(&self, source: PaneId, target: PaneId, request: SplitRequest) -> bool {
        if source == target || self.get_zoomed_pane().is_some() {
            return false;
        }
        let panes = self.iter_panes();
        if !panes.iter().any(|p| p.pane.pane_id() == source) {
            return false;
        }
        let Some(target_pos) = panes.iter().find(|p| p.pane.pane_id() == target) else {
            return false;
        };
        // Removing the source only ever gives the target more room, so a
        // target that can be split now can be split afterwards. Checking here
        // keeps the pane from being taken out when the insert would then fail.
        let extent = match request.direction {
            SplitDirection::Horizontal => target_pos.width,
            SplitDirection::Vertical => target_pos.height,
        };
        if extent < 3 {
            return false;
        }

        let Some(pane) = self.remove_pane(source) else {
            return false;
        };
        let docked = self
            .iter_panes()
            .iter()
            .find(|p| p.pane.pane_id() == target)
            .map(|p| p.index)
            .ok_or_else(|| anyhow::anyhow!("target pane vanished"))
            .and_then(|idx| self.split_and_insert(idx, request, Arc::clone(&pane)));
        match docked {
            Ok(_) => {
                self.set_active_pane(&pane);
                true
            }
            Err(err) => {
                // Never lose a running pane: put it back somewhere.
                log::error!("dock_pane failed, restoring the pane: {:#}", err);
                // Index 0 may be a hidden pane; put it back beside one on show.
                let visible = self.iter_panes().first().map_or(0, |p| p.index);
                if let Err(err) = self.split_and_insert(visible, SplitRequest::default(), pane) {
                    log::error!("dock_pane could not restore the pane: {:#}", err);
                }
                false
            }
        }
    }

    /// Adjusts the size of the active pane in the specified direction
    /// by the specified amount.
    pub fn adjust_pane_size(&self, direction: PaneDirection, amount: usize) {
        let mut inner = self.inner.lock();
        inner.adjust_pane_size(direction, amount);
        inner.remember_focus_width();
    }

    /// Activate an adjacent pane in the specified direction.
    /// In cases where there are multiple adjacent panes in the
    /// intended direction, we take the pane that has the largest
    /// edge intersection.
    pub fn activate_pane_direction(&self, direction: PaneDirection) {
        self.inner.lock().activate_pane_direction(direction)
    }

    /// Returns an adjacent pane in the specified direction.
    /// In cases where there are multiple adjacent panes in the
    /// intended direction, we take the pane that has the largest
    /// edge intersection.
    pub fn get_pane_direction(&self, direction: PaneDirection, ignore_zoom: bool) -> Option<usize> {
        self.inner.lock().get_pane_direction(direction, ignore_zoom)
    }

    pub fn prune_dead_panes(&self) -> bool {
        self.inner.lock().prune_dead_panes()
    }

    pub fn kill_pane(&self, pane_id: PaneId) -> bool {
        self.inner.lock().kill_pane(pane_id)
    }

    pub fn kill_panes_in_domain(&self, domain: DomainId) -> bool {
        self.inner.lock().kill_panes_in_domain(domain)
    }

    /// Remove pane from tab.
    /// The pane is still live in the mux; the intent is for the pane to
    /// be added to a different tab.
    pub fn remove_pane(&self, pane_id: PaneId) -> Option<Arc<dyn Pane>> {
        self.inner.lock().remove_pane(pane_id)
    }

    pub fn can_close_without_prompting(&self, reason: CloseReason) -> bool {
        self.inner.lock().can_close_without_prompting(reason)
    }

    pub fn is_dead(&self) -> bool {
        self.inner.lock().is_dead()
    }

    pub fn get_active_pane(&self) -> Option<Arc<dyn Pane>> {
        self.inner.lock().get_active_pane()
    }

    #[allow(unused)]
    pub fn get_active_idx(&self) -> usize {
        self.inner.lock().get_active_idx()
    }

    pub fn set_active_pane(&self, pane: &Arc<dyn Pane>) {
        self.inner.lock().set_active_pane(pane)
    }

    pub fn set_active_idx(&self, pane_index: usize) {
        self.inner.lock().set_active_idx(pane_index)
    }

    /// Assigns the root pane.
    /// This is suitable when creating a new tab and then assigning
    /// the initial pane
    pub fn assign_pane(&self, pane: &Arc<dyn Pane>) {
        self.inner.lock().assign_pane(pane)
    }

    /// Swap the active pane with the specified pane_index
    pub fn swap_active_with_index(&self, pane_index: usize, keep_focus: bool) -> Option<()> {
        self.inner
            .lock()
            .swap_active_with_index(pane_index, keep_focus)
    }

    /// Computes the size of the pane that would result if the specified
    /// pane was split in a particular direction.
    /// The intent is to call this prior to spawning the new pane so that
    /// you can create it with the correct size.
    /// May return None if the specified pane_index is invalid.
    pub fn compute_split_size(
        &self,
        pane_index: usize,
        request: SplitRequest,
    ) -> Option<SplitDirectionAndSize> {
        self.inner.lock().compute_split_size(pane_index, request)
    }

    /// Split the pane that has pane_index in the given direction and assign
    /// the right/bottom pane of the newly created split to the provided Pane
    /// instance.  Returns the resultant index of the newly inserted pane.
    /// Both the split and the inserted pane will be resized.
    pub fn split_and_insert(
        &self,
        pane_index: usize,
        request: SplitRequest,
        pane: Arc<dyn Pane>,
    ) -> anyhow::Result<usize> {
        self.inner
            .lock()
            .split_and_insert(pane_index, request, pane)
    }

    pub fn get_zoomed_pane(&self) -> Option<Arc<dyn Pane>> {
        self.inner.lock().get_zoomed_pane()
    }
}

impl TabInner {
    fn new(size: &TerminalSize) -> Self {
        Self {
            id: TAB_ID.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed),
            pane: Some(Tree::new()),
            size: *size,
            size_before_zoom: *size,
            active: 0,
            zoomed: None,
            title: String::new(),
            recency: Recency::default(),
            hidden: HashSet::new(),
            focus_arranged: false,
            groups: GroupState::default(),
            docking: false,
        }
    }

    fn sync_with_pane_tree<F>(&mut self, size: TerminalSize, root: PaneNode, mut make_pane: F)
    where
        F: FnMut(PaneEntry) -> Arc<dyn Pane>,
    {
        let mut active = None;
        let mut zoomed = None;

        log::debug!("sync_with_pane_tree with size {:?}", size);

        let t = build_from_pane_tree(root.into_tree(), &mut active, &mut zoomed, &mut make_pane);
        let mut cursor = t.cursor();

        self.active = 0;
        if let Some(active) = active {
            // Resolve the active pane to its index
            let mut index = 0;
            loop {
                if let Some(pane) = cursor.leaf_mut() {
                    if active.pane_id() == pane.pane_id() {
                        // Found it
                        self.active = index;
                        self.recency.tag(index);
                        break;
                    }
                    index += 1;
                }
                match cursor.preorder_next() {
                    Ok(c) => cursor = c,
                    Err(c) => {
                        // Didn't find it
                        cursor = c;
                        break;
                    }
                }
            }
        }
        self.pane.replace(cursor.tree());
        self.zoomed = zoomed;
        self.size = size;

        self.resize(size);

        log::debug!(
            "sync tab: {:#?} zoomed: {} {:#?}",
            size,
            self.zoomed.is_some(),
            self.iter_panes()
        );
        assert!(self.pane.is_some());
    }

    fn codec_pane_tree(&mut self) -> PaneNode {
        let mux = Mux::get();
        let tab_id = self.id;
        let window_id = match mux.window_containing_tab(tab_id) {
            Some(w) => w,
            None => {
                log::error!("no window contains tab {}", tab_id);
                return PaneNode::Empty;
            }
        };

        let workspace = match mux
            .get_window(window_id)
            .map(|w| w.get_workspace().to_string())
        {
            Some(ws) => ws,
            None => {
                log::error!("window id {} doesn't have a window!?", window_id);
                return PaneNode::Empty;
            }
        };

        let active = self.get_active_pane();
        let zoomed = self.zoomed.as_ref();
        if let Some(root) = self.pane.as_ref() {
            pane_tree(
                root,
                tab_id,
                window_id,
                active.as_ref(),
                zoomed,
                &workspace,
                0,
                0,
            )
        } else {
            PaneNode::Empty
        }
    }

    fn session_layout_tree(&mut self) -> PaneLayoutNode {
        let active = self.get_active_pane();
        match self.pane.as_ref() {
            Some(tree) => {
                convert_tree_to_session_layout(tree, active.as_ref(), &self.hidden, &self.groups)
            }
            None => PaneLayoutNode::Pane {
                cwd: None,
                command: None,
                hidden: false,
                is_active: false,
                group: None,
                stack: None,
                stack_pos: None,
            },
        }
    }

    /// Returns a count of how many panes are in this tab
    fn count_panes(&mut self) -> usize {
        let mut count = 0;
        let mut cursor = self.pane.take().unwrap().cursor();

        loop {
            if cursor.is_leaf() {
                count += 1;
            }
            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    return count;
                }
            }
        }
    }

    /// Sets the zoom state, returns the prior state
    fn set_zoomed(&mut self, zoomed: bool) -> bool {
        if self.zoomed.is_some() == zoomed {
            // Current zoom state matches intended zoom state,
            // so we have nothing to do.
            return zoomed;
        }
        self.toggle_zoom();
        !zoomed
    }

    fn toggle_zoom(&mut self) {
        let size = self.size;
        if self.zoomed.take().is_some() {
            // We were zoomed, but now we are not.
            // Re-apply the size to the panes
            if let Some(pane) = self.get_active_pane() {
                pane.set_zoomed(false);
            }
            self.size = self.size_before_zoom;
            self.resize(size);
        } else {
            // We weren't zoomed, but now we want to zoom.
            // Locate the active pane
            self.size_before_zoom = size;
            if let Some(pane) = self.get_active_pane() {
                pane.set_zoomed(true);
                pane.resize(size).ok();
                self.zoomed.replace(pane);
            }
        }
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    /// True when any pane group or stack exists: tree operations that move
    /// panes between places would break their contiguous subtrees.
    fn has_groups(&self) -> bool {
        !self.groups.groups.is_empty() || !self.groups.stacks.is_empty()
    }

    /// Forget panes that were removed from the tree: drop them from the
    /// hidden set and the group model, show the unit a stack falls back to,
    /// and make sure focus is on a visible pane.
    fn prune_removed_panes(&mut self, removed: &[PaneId]) {
        let mut changed = false;
        let mut reveal = vec![];
        for id in removed {
            changed |= self.hidden.remove(id);
            let removal = self.groups.remove_pane(*id);
            if let Some(unit) = removal.reveal {
                reveal.push(unit);
            }
        }
        for unit in &reveal {
            self.set_unit_hidden(unit, false);
        }
        if self.pane.as_ref().map_or(true, |t| matches!(t, Tree::Empty)) {
            return;
        }
        if changed || !reveal.is_empty() {
            self.refresh_after_unit_change();
        }

        // Removing panes shifts the active index; it may now name a pane
        // that is hidden.
        let active_hidden = self
            .get_active_pane()
            .map_or(false, |p| self.hidden.contains(&p.pane_id()));
        if active_hidden {
            let target = reveal
                .first()
                .and_then(|unit| self.unit_focus_pane(unit))
                .or_else(|| self.iter_panes().first().map(|p| p.pane.pane_id()));
            if let Some(id) = target {
                self.focus_pane_id(id);
            }
        }
        self.debug_validate_groups();
    }

    fn is_spatial(direction: PaneDirection) -> bool {
        matches!(
            direction,
            PaneDirection::Up | PaneDirection::Down | PaneDirection::Left | PaneDirection::Right
        )
    }

    /// The visible pane next to the active one in `direction`.
    fn neighbour_pane_id(&mut self, direction: PaneDirection) -> Option<PaneId> {
        let index = self.get_pane_direction(direction, false)?;
        self.iter_panes()
            .into_iter()
            .find(|p| p.index == index)
            .map(|p| p.pane.pane_id())
    }

    /// The pane that takes focus when `unit` is focused.
    fn unit_focus_pane(&self, unit: &Unit) -> Option<PaneId> {
        match unit {
            Unit::Pane(id) => Some(*id),
            Unit::Group(gid) => self
                .groups
                .groups
                .iter()
                .find(|g| g.id == *gid)
                .map(|g| g.last_active),
        }
    }

    fn focus_pane_id(&mut self, id: PaneId) {
        let pane = self
            .iter_panes_ignoring_zoom()
            .into_iter()
            .find(|p| p.pane.pane_id() == id)
            .map(|p| p.pane);
        if let Some(pane) = pane {
            self.set_active_pane(&pane);
        }
    }

    /// Show or hide every pane of `unit`. Edits the hidden set directly:
    /// `toggle_pane_hidden` moves one pane at a time and refuses to hide the
    /// last visible pane, which a unit swap passes through.
    fn set_unit_hidden(&mut self, unit: &Unit, hidden: bool) {
        for id in self.groups.unit_panes(unit) {
            if hidden {
                self.hidden.insert(id);
            } else {
                self.hidden.remove(&id);
            }
        }
    }

    fn debug_validate_groups(&self) {
        if !cfg!(debug_assertions) {
            return;
        }
        if let Some(tree) = &self.pane {
            if let Err(err) = self.groups.validate(&subtree_leaf_sets(tree), &self.hidden) {
                log::error!("pane group invariant broken: {}", err);
                debug_assert!(false, "pane group invariant broken: {}", err);
            }
        }
    }

    fn refresh_after_unit_change(&mut self) {
        let size = self.size;
        self.resize(size);
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    fn restore_groups(&mut self, layout: &PaneLayoutNode) -> bool {
        let leaves = layout_leaves(layout);
        let panes: Vec<PaneId> = self
            .iter_panes_ignoring_zoom()
            .iter()
            .map(|p| p.pane.pane_id())
            .collect();
        if leaves.len() != panes.len() {
            return false;
        }

        let mut group_members: Vec<(u32, Vec<PaneId>)> = vec![];
        let mut stack_units: Vec<(u32, Vec<(u32, Vec<PaneId>)>)> = vec![];
        for (leaf, pane) in leaves.iter().zip(&panes) {
            if let Some(group) = leaf.group {
                match group_members.iter_mut().find(|(id, _)| *id == group) {
                    Some((_, members)) => members.push(*pane),
                    None => group_members.push((group, vec![*pane])),
                }
            }
            if let (Some(stack), Some(pos)) = (leaf.stack, leaf.stack_pos) {
                let units = match stack_units.iter_mut().find(|(id, _)| *id == stack) {
                    Some((_, units)) => units,
                    None => {
                        stack_units.push((stack, vec![]));
                        &mut stack_units.last_mut().unwrap().1
                    }
                };
                match units.iter_mut().find(|(p, _)| *p == pos) {
                    Some((_, members)) => members.push(*pane),
                    None => units.push((pos, vec![*pane])),
                }
            }
        }

        let mut state = GroupState::default();
        for (_, members) in group_members {
            state.create_group(members);
        }
        for (_, mut units) in stack_units {
            units.sort_by_key(|(pos, _)| *pos);
            let units: Vec<Unit> = units
                .iter()
                .map(|(_, members)| state.unit_of(members[0]))
                .collect();
            let visible: Vec<usize> = units
                .iter()
                .enumerate()
                .filter(|(_, unit)| {
                    state
                        .unit_panes(unit)
                        .iter()
                        .all(|p| !self.hidden.contains(p))
                })
                .map(|(index, _)| index)
                .collect();
            let [active] = visible[..] else {
                log::warn!("saved stack has {} visible units; dropping groups", visible.len());
                return false;
            };
            state.stacks.push(PaneStack { units, active });
        }

        let valid = self.pane.as_ref().map_or(false, |tree| {
            match state.validate(&subtree_leaf_sets(tree), &self.hidden) {
                Ok(()) => true,
                Err(err) => {
                    log::warn!("saved pane groups do not fit the layout: {}", err);
                    false
                }
            }
        });
        if !valid {
            return false;
        }
        self.groups = state;
        if let Some(active) = self.get_active_pane().map(|p| p.pane_id()) {
            self.groups.note_active(active);
        }
        true
    }

    fn group_panes(&mut self, direction: PaneDirection) -> bool {
        if self.zoomed.is_some() || !Self::is_spatial(direction) {
            return false;
        }
        let Some(active) = self.get_active_pane().map(|p| p.pane_id()) else {
            return false;
        };
        let Some(neighbour) = self.neighbour_pane_id(direction) else {
            return false;
        };
        self.group_units(active, neighbour, direction)
    }

    fn pane_unit_info(&self) -> HashMap<PaneId, PaneUnitInfo> {
        let ids = self.pane.as_ref().map(leaf_order).unwrap_or_default();
        ids.into_iter()
            .map(|id| {
                let unit = self.groups.unit_of(id);
                let stack = self.groups.stack_of(&unit);
                let on_show = !self.hidden.contains(&id);
                let info = PaneUnitInfo {
                    group: match unit {
                        Unit::Group(g) => Some(g),
                        Unit::Pane(_) => None,
                    },
                    stack,
                    stack_len: stack.map_or(1, |i| self.groups.stacks[i].units.len()),
                    on_show,
                };
                (id, info)
            })
            .collect()
    }

    fn group_with(&mut self, target: PaneId) -> bool {
        let Some((active, direction)) = self.direction_to_visible(target) else {
            return false;
        };
        self.group_units(active, target, direction)
    }

    /// The active pane and the compass direction from it to the visible pane
    /// `target`; None for the active pane itself, a hidden or unknown pane,
    /// or a zoomed tab.
    fn direction_to_visible(&mut self, target: PaneId) -> Option<(PaneId, PaneDirection)> {
        if self.zoomed.is_some() {
            return None;
        }
        let active = self.get_active_pane()?.pane_id();
        if active == target {
            return None;
        }
        let panes = self.iter_panes();
        let from = panes.iter().find(|p| p.pane.pane_id() == active)?;
        let to = panes.iter().find(|p| p.pane.pane_id() == target)?;
        let dx = to.left as isize - from.left as isize;
        let dy = to.top as isize - from.top as isize;
        let direction = if dx.abs() >= dy.abs() {
            if dx >= 0 { PaneDirection::Right } else { PaneDirection::Left }
        } else if dy >= 0 {
            PaneDirection::Down
        } else {
            PaneDirection::Up
        };
        Some((active, direction))
    }

    fn group_units(&mut self, active: PaneId, neighbour: PaneId, direction: PaneDirection) -> bool {
        let a = self.groups.unit_of(active);
        let b = self.groups.unit_of(neighbour);
        if a == b || self.groups.stack_of(&a).is_some() || self.groups.stack_of(&b).is_some() {
            return false;
        }
        let panes_a = self.groups.unit_panes(&a);
        let panes_b = self.groups.unit_panes(&b);
        let union: HashSet<PaneId> = panes_a.iter().chain(panes_b.iter()).copied().collect();
        let adjacent = self
            .pane
            .as_ref()
            .map_or(false, |tree| subtree_leaf_sets(tree).contains(&union));
        if !adjacent && !self.dock_unit(&panes_a, &panes_b, dock_request(direction, 50)) {
            return false;
        }

        for unit in [&a, &b] {
            if let Unit::Group(id) = unit {
                self.groups.dissolve(*id);
            }
        }
        let mut members = panes_a;
        members.extend(panes_b);
        let id = self.groups.create_group(members);
        if let Some(group) = self.groups.groups.iter_mut().find(|g| g.id == id) {
            group.last_active = active;
        }
        self.focus_pane_id(active);
        self.debug_validate_groups();
        true
    }

    fn ungroup_active(&mut self) -> bool {
        let Some(active) = self.get_active_pane().map(|p| p.pane_id()) else {
            return false;
        };
        match self.groups.unit_of(active) {
            Unit::Group(id) if self.groups.stack_of(&Unit::Group(id)).is_none() => {
                self.groups.dissolve(id);
                self.debug_validate_groups();
                true
            }
            _ => false,
        }
    }

    fn stack_join(&mut self, direction: PaneDirection) -> bool {
        if self.zoomed.is_some() || !Self::is_spatial(direction) {
            return false;
        }
        let Some(active) = self.get_active_pane().map(|p| p.pane_id()) else {
            return false;
        };
        let Some(neighbour) = self.neighbour_pane_id(direction) else {
            return false;
        };
        self.stack_units(active, neighbour)
    }

    fn stack_join_with(&mut self, target: PaneId) -> bool {
        let Some((active, _)) = self.direction_to_visible(target) else {
            return false;
        };
        self.stack_units(active, target)
    }

    fn stack_units(&mut self, active: PaneId, neighbour: PaneId) -> bool {
        let a = self.groups.unit_of(active);
        let b = self.groups.unit_of(neighbour);
        if a == b || self.groups.stack_of(&a).is_some() {
            return false;
        }
        let panes_a = self.groups.unit_panes(&a);
        let panes_b = self.groups.unit_panes(&b);
        // The joined unit takes half of the neighbour's slot; hiding the
        // neighbour then gives it the whole slot.
        let request = SplitRequest {
            direction: SplitDirection::Horizontal,
            target_is_second: true,
            top_level: false,
            size: SplitSize::Percent(50),
        };
        if !self.dock_unit(&panes_a, &panes_b, request) {
            return false;
        }

        match self.groups.stack_of(&b) {
            Some(index) => {
                let stack = &mut self.groups.stacks[index];
                stack.units.push(a.clone());
                stack.active = stack.units.len() - 1;
            }
            None => self.groups.stacks.push(PaneStack {
                units: vec![b.clone(), a.clone()],
                active: 1,
            }),
        }
        self.set_unit_hidden(&b, true);
        self.refresh_after_unit_change();
        if let Some(id) = self.unit_focus_pane(&a) {
            self.focus_pane_id(id);
        }
        self.debug_validate_groups();
        true
    }

    /// Focusing a pane that is hidden inside a stack brings its unit on show.
    fn reveal_stacked_pane(&mut self, pane: PaneId) {
        if !self.hidden.contains(&pane) {
            return;
        }
        let unit = self.groups.unit_of(pane);
        self.show_stack_unit(&unit);
    }

    /// Put `unit` on show in its stack, hiding the unit that was there.
    /// Returns false when `unit` is in no stack or already on show.
    fn show_stack_unit(&mut self, unit: &Unit) -> bool {
        let Some(index) = self.groups.stack_of(unit) else {
            return false;
        };
        let stack = &self.groups.stacks[index];
        let Some(to) = stack.units.iter().position(|u| u == unit) else {
            return false;
        };
        if to == stack.active {
            return false;
        }
        let old = stack.units[stack.active].clone();
        // Show the new unit before hiding the old one: the tab never holds
        // zero visible panes.
        self.set_unit_hidden(unit, false);
        self.set_unit_hidden(&old, true);
        self.groups.stacks[index].active = to;
        self.refresh_after_unit_change();
        true
    }

    fn stack_cycle(&mut self, direction: PaneDirection) -> bool {
        let forward = match direction {
            PaneDirection::Next => true,
            PaneDirection::Prev => false,
            _ => return false,
        };
        let Some(active) = self.get_active_pane().map(|p| p.pane_id()) else {
            return false;
        };
        let current = self.groups.unit_of(active);
        let Some(index) = self.groups.stack_of(&current) else {
            return false;
        };
        let stack = &self.groups.stacks[index];
        let count = stack.units.len();
        if count < 2 {
            return false;
        }
        let from = stack.active;
        let to = if forward { (from + 1) % count } else { (from + count - 1) % count };
        let new = stack.units[to].clone();

        self.set_zoomed(false);
        self.show_stack_unit(&new);
        if let Some(id) = self.unit_focus_pane(&new) {
            self.focus_pane_id(id);
        }
        self.debug_validate_groups();
        true
    }

    fn dock_unit(&mut self, source: &[PaneId], target: &[PaneId], request: SplitRequest) -> bool {
        if self.zoomed.is_some() || source.is_empty() || target.is_empty() {
            return false;
        }
        let source_set: HashSet<PaneId> = source.iter().copied().collect();
        let target_set: HashSet<PaneId> = target.iter().copied().collect();
        if source_set.len() != source.len() || !source_set.is_disjoint(&target_set) {
            return false;
        }
        let Some(tree) = self.pane.as_ref() else {
            return false;
        };
        if !subtree_leaf_sets(tree).contains(&target_set) {
            return false;
        }
        let all = self.iter_panes_ignoring_zoom();
        let mut panes = vec![];
        for id in source {
            match all.iter().find(|p| p.pane.pane_id() == *id) {
                Some(p) => panes.push(Arc::clone(&p.pane)),
                None => return false,
            }
        }
        if !target.iter().all(|id| all.iter().any(|p| p.pane.pane_id() == *id)) {
            return false;
        }

        // Taking the source out only gives room back, and leaves the target
        // subtree intact because the source panes are not inside it.
        self.docking = true;
        for id in source {
            self.remove_pane(*id);
        }
        self.docking = false;
        let positions = self.iter_panes();
        let (mut left, mut top, mut right, mut bottom) = (usize::MAX, usize::MAX, 0, 0);
        for p in positions.iter().filter(|p| target_set.contains(&p.pane.pane_id())) {
            left = left.min(p.left);
            top = top.min(p.top);
            right = right.max(p.left + p.width);
            bottom = bottom.max(p.top + p.height);
        }
        let (width, height) = (right.saturating_sub(left), bottom.saturating_sub(top));
        let cell_dims = self.cell_dimensions();
        let data = split_size_for_extent(&cell_dims, width, height, request);
        let docked_size = if request.target_is_second {
            &data.second
        } else {
            &data.first
        };
        let (docked_cols, docked_rows) = (docked_size.cols, docked_size.rows);

        let docked_tree = chain_subtree(&panes, docked_cols, docked_rows, &cell_dims);
        let mut docked = Some(docked_tree);
        let (tree, found) = replace_subtree_with_pair(
            self.pane.take().unwrap(),
            &target_set,
            &mut docked,
            &data,
            request.target_is_second,
        );
        self.pane.replace(tree);
        if !found {
            // Never lose a running pane: put them back somewhere.
            log::error!("dock_unit could not find the target subtree");
            for pane in panes {
                let visible = self.iter_panes().first().map_or(0, |p| p.index);
                if let Err(err) = self.split_and_insert(visible, SplitRequest::default(), pane) {
                    log::error!("dock_unit could not restore a pane: {:#}", err);
                }
            }
            return false;
        }

        self.focus_arranged = false;
        let size = self.size;
        self.resize(size);
        self.set_active_pane(&panes[0]);
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
        true
    }

    fn toggle_pane_hidden(&mut self, pane_id: PaneId) {
        // A stacked pane is shown or hidden with its whole unit, by swapping
        // the stack: hiding one pane of a unit would split the unit.
        let unit = self.groups.unit_of(pane_id);
        if self.groups.stack_of(&unit).is_some() {
            if self.hidden.contains(&pane_id) {
                if self.show_stack_unit(&unit) {
                    if let Some(id) = self.unit_focus_pane(&unit) {
                        self.focus_pane_id(id);
                    }
                }
            } else {
                log::warn!("pane {} is on show in a stack; cycle the stack instead", pane_id);
            }
            return;
        }
        if self.hidden.contains(&pane_id) {
            // Unhide
            self.hidden.remove(&pane_id);
        } else {
            // Don't allow hiding if it would leave zero visible panes
            let all_panes = self.iter_panes_ignoring_zoom();
            let visible_count = all_panes
                .iter()
                .filter(|p| !self.hidden.contains(&p.pane.pane_id()) && p.pane.pane_id() != pane_id)
                .count();
            if visible_count == 0 {
                log::warn!("Cannot hide pane {} — it is the last visible pane", pane_id);
                return;
            }

            // If hiding the active pane, switch to the next visible pane first
            let active_pane_id = self.get_active_pane().map(|p| p.pane_id());
            if active_pane_id == Some(pane_id) {
                // Find next visible pane
                if let Some(next) = all_panes.iter().find(|p| {
                    p.pane.pane_id() != pane_id && !self.hidden.contains(&p.pane.pane_id())
                }) {
                    self.active = next.index;
                }
            }

            // If hiding the zoomed pane, un-zoom first
            if self.zoomed.as_ref().map(|p| p.pane_id()) == Some(pane_id) {
                self.toggle_zoom();
            }

            self.hidden.insert(pane_id);
        }

        // Re-apply sizes so the visible panes fill the available space
        let size = self.size;
        self.resize(size);
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    fn contains_pane(&self, pane: PaneId) -> bool {
        fn contains(tree: &Tree, pane: PaneId) -> bool {
            match tree {
                Tree::Empty => false,
                Tree::Node { left, right, .. } => contains(left, pane) || contains(right, pane),
                Tree::Leaf(p) => p.pane_id() == pane,
            }
        }
        match &self.pane {
            Some(root) => contains(root, pane),
            None => false,
        }
    }

    /// Walks the pane tree to produce the topologically ordered flattened
    /// list of PositionedPane instances along with their positioning information.
    /// Hidden panes are excluded; their siblings expand to fill the space.
    fn iter_panes(&mut self) -> Vec<PositionedPane> {
        if !self.hidden.is_empty() {
            return self.iter_panes_with_hidden(true);
        }
        self.iter_panes_impl(true)
    }

    /// Like iter_panes, except that it will include all panes, regardless of
    /// whether one of them is currently zoomed or hidden.
    fn iter_panes_ignoring_zoom(&mut self) -> Vec<PositionedPane> {
        self.iter_panes_impl(false)
    }

    fn rotate_counter_clockwise(&mut self) {
        if self.has_groups() {
            return;
        }
        let panes = self.iter_panes_ignoring_zoom();
        if panes.is_empty() {
            // Shouldn't happen, but we check for this here so that the
            // expect below cannot trigger a panic
            return;
        }
        let mut pane_to_swap = panes
            .first()
            .map(|p| p.pane.clone())
            .expect("at least one pane");

        let mut cursor = self.pane.take().unwrap().cursor();

        loop {
            if cursor.is_leaf() {
                std::mem::swap(&mut pane_to_swap, cursor.leaf_mut().unwrap());
            }

            match cursor.postorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    let size = self.size;
                    apply_sizes_from_splits(self.pane.as_mut().unwrap(), &size);
                    break;
                }
            }
        }
    }

    fn rotate_clockwise(&mut self) {
        if self.has_groups() {
            return;
        }
        let panes = self.iter_panes_ignoring_zoom();
        if panes.is_empty() {
            // Shouldn't happen, but we check for this here so that the
            // expect below cannot trigger a panic
            return;
        }
        let mut pane_to_swap = panes
            .last()
            .map(|p| p.pane.clone())
            .expect("at least one pane");

        let mut cursor = self.pane.take().unwrap().cursor();

        loop {
            if cursor.is_leaf() {
                std::mem::swap(&mut pane_to_swap, cursor.leaf_mut().unwrap());
            }

            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    let size = self.size;
                    apply_sizes_from_splits(self.pane.as_mut().unwrap(), &size);
                    break;
                }
            }
        }
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    fn iter_panes_impl(&mut self, respect_zoom_state: bool) -> Vec<PositionedPane> {
        let mut panes = vec![];

        if respect_zoom_state {
            if let Some(zoomed) = self.zoomed.as_ref() {
                let size = self.size;
                panes.push(PositionedPane {
                    index: 0,
                    is_active: true,
                    is_zoomed: true,
                    left: 0,
                    top: 0,
                    width: size.cols.into(),
                    pixel_width: size.pixel_width.into(),
                    height: size.rows.into(),
                    pixel_height: size.pixel_height.into(),
                    pane: Arc::clone(zoomed),
                });
                return panes;
            }
        }

        let active_idx = self.active;
        let zoomed_id = self.zoomed.as_ref().map(|p| p.pane_id());
        let root_size = self.size;
        let mut cursor = self.pane.take().unwrap().cursor();

        loop {
            if cursor.is_leaf() {
                let index = panes.len();
                let mut left = 0usize;
                let mut top = 0usize;
                let mut parent_size = None;
                for (branch, node) in cursor.path_to_root() {
                    if let Some(node) = node {
                        if parent_size.is_none() {
                            parent_size.replace(if branch == PathBranch::IsRight {
                                node.second
                            } else {
                                node.first
                            });
                        }
                        if branch == PathBranch::IsRight {
                            top += node.top_of_second();
                            left += node.left_of_second();
                        }
                    }
                }

                let pane = Arc::clone(cursor.leaf_mut().unwrap());
                let dims = parent_size.unwrap_or_else(|| root_size);

                panes.push(PositionedPane {
                    index,
                    is_active: index == active_idx,
                    is_zoomed: zoomed_id == Some(pane.pane_id()),
                    left,
                    top,
                    width: dims.cols as _,
                    height: dims.rows as _,
                    pixel_width: dims.pixel_width as _,
                    pixel_height: dims.pixel_height as _,
                    pane,
                });
            }

            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    break;
                }
            }
        }

        panes
    }

    /// Walk the pane tree, skipping hidden panes and giving their space to siblings.
    /// Uses a recursive approach rather than cursor to correctly reallocate sizes.
    fn iter_panes_with_hidden(&mut self, respect_zoom_state: bool) -> Vec<PositionedPane> {
        let mut panes = vec![];

        if respect_zoom_state {
            if let Some(zoomed) = self.zoomed.as_ref() {
                let size = self.size;
                panes.push(PositionedPane {
                    index: 0,
                    is_active: true,
                    is_zoomed: true,
                    left: 0,
                    top: 0,
                    width: size.cols.into(),
                    pixel_width: size.pixel_width.into(),
                    height: size.rows.into(),
                    pixel_height: size.pixel_height.into(),
                    pane: Arc::clone(zoomed),
                });
                return panes;
            }
        }

        let active_idx = self.active;
        let zoomed_id = self.zoomed.as_ref().map(|p| p.pane_id());
        let root_size = self.size;
        let hidden = &self.hidden;

        if let Some(tree) = &self.pane {
            let order = leaf_order(tree);
            collect_visible_panes(
                tree,
                &root_size,
                0,     // left
                0,     // top
                active_idx,
                zoomed_id,
                hidden,
                &order,
                &mut panes,
            );
        }

        panes
    }

    fn iter_splits(&mut self) -> Vec<PositionedSplit> {
        let mut dividers = vec![];
        if self.zoomed.is_some() {
            return dividers;
        }

        if !self.hidden.is_empty() {
            // Use recursive approach to skip splits where one side is entirely hidden
            if let Some(tree) = &self.pane {
                let mut index = 0;
                collect_visible_splits(tree, 0, 0, &self.hidden, &mut dividers, &mut index);
            }
            return dividers;
        }

        let mut cursor = self.pane.take().unwrap().cursor();
        let mut index = 0;

        loop {
            if !cursor.is_leaf() {
                let mut left = 0usize;
                let mut top = 0usize;
                for (branch, p) in cursor.path_to_root() {
                    if let Some(p) = p {
                        if branch == PathBranch::IsRight {
                            left += p.left_of_second();
                            top += p.top_of_second();
                        }
                    }
                }
                if let Ok(Some(node)) = cursor.node_mut() {
                    match node.direction {
                        SplitDirection::Horizontal => left += node.first.cols as usize,
                        SplitDirection::Vertical => top += node.first.rows as usize,
                    }

                    dividers.push(PositionedSplit {
                        index,
                        direction: node.direction,
                        left,
                        top,
                        size: if node.direction == SplitDirection::Horizontal {
                            node.height() as usize
                        } else {
                            node.width() as usize
                        },
                    })
                }
                index += 1;
            }

            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    break;
                }
            }
        }

        dividers
    }

    fn get_size(&self) -> TerminalSize {
        self.size
    }

    fn resize(&mut self, size: TerminalSize) {
        if size.rows == 0 || size.cols == 0 {
            // Ignore "impossible" resize requests
            return;
        }

        if let Some(zoomed) = &self.zoomed {
            self.size = size;
            zoomed.resize(size).ok();
        } else {
            let dims = cell_dimensions(&size);
            let (min_x, min_y) = compute_min_size(self.pane.as_mut().unwrap());
            let current_size = self.size;

            // Constrain the new size to the minimum possible dimensions
            let cols = size.cols.max(min_x);
            let rows = size.rows.max(min_y);
            let size = TerminalSize {
                rows,
                cols,
                pixel_width: cols * dims.pixel_width,
                pixel_height: rows * dims.pixel_height,
                dpi: dims.dpi,
            };

            // Update the split nodes with adjusted sizes
            adjust_x_size(
                self.pane.as_mut().unwrap(),
                cols as isize - current_size.cols as isize,
                &dims,
            );
            adjust_y_size(
                self.pane.as_mut().unwrap(),
                rows as isize - current_size.rows as isize,
                &dims,
            );

            self.size = size;

            // And then resize the individual panes to match
            apply_sizes_from_splits_with_hidden(self.pane.as_mut().unwrap(), &size, &self.hidden);
        }

        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    fn apply_pane_size(&mut self, pane_size: TerminalSize, cursor: &mut Cursor) {
        let cell_width = pane_size
            .pixel_width
            .checked_div(pane_size.cols)
            .unwrap_or(1);
        let cell_height = pane_size
            .pixel_height
            .checked_div(pane_size.rows)
            .unwrap_or(1);
        if let Ok(Some(node)) = cursor.node_mut() {
            // Adjust the size of the node; we preserve the size of the first
            // child and adjust the second, so if we are split down the middle
            // and the window is made wider, the right column will grow in
            // size, leaving the left at its current width.
            if node.direction == SplitDirection::Horizontal {
                node.first.rows = pane_size.rows;
                node.second.rows = pane_size.rows;

                node.second.cols = pane_size.cols.saturating_sub(1 + node.first.cols);
            } else {
                node.first.cols = pane_size.cols;
                node.second.cols = pane_size.cols;

                node.second.rows = pane_size.rows.saturating_sub(1 + node.first.rows);
            }
            node.first.pixel_width = node.first.cols * cell_width;
            node.first.pixel_height = node.first.rows * cell_height;

            node.second.pixel_width = node.second.cols * cell_width;
            node.second.pixel_height = node.second.rows * cell_height;
        }
    }

    fn rebuild_splits_sizes_from_contained_panes(&mut self) {
        if self.zoomed.is_some() {
            return;
        }

        fn compute_size(node: &mut Tree) -> Option<TerminalSize> {
            match node {
                Tree::Empty => None,
                Tree::Leaf(pane) => {
                    let dims = pane.get_dimensions();
                    let size = TerminalSize {
                        cols: dims.cols,
                        rows: dims.viewport_rows,
                        pixel_height: dims.pixel_height,
                        pixel_width: dims.pixel_width,
                        dpi: dims.dpi,
                    };
                    Some(size)
                }
                Tree::Node { left, right, data } => {
                    if let Some(data) = data {
                        if let Some(first) = compute_size(left) {
                            data.first = first;
                        }
                        if let Some(second) = compute_size(right) {
                            data.second = second;
                        }
                        Some(data.size())
                    } else {
                        None
                    }
                }
            }
        }

        if let Some(root) = self.pane.as_mut() {
            if let Some(size) = compute_size(root) {
                self.size = size;
            }
        }
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    fn resize_split_by(&mut self, split_index: usize, delta: isize) {
        if self.zoomed.is_some() {
            return;
        }

        let mut cursor = self.pane.take().unwrap().cursor();
        let mut index = 0;

        // Position cursor on the specified split
        loop {
            if !cursor.is_leaf() {
                if index == split_index {
                    // Found it
                    break;
                }
                index += 1;
            }
            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    // Didn't find it
                    self.pane.replace(c.tree());
                    return;
                }
            }
        }

        // Now cursor is looking at the split
        self.adjust_node_at_cursor(&mut cursor, delta);
        self.cascade_size_from_cursor(cursor);
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    /// Flip the direction of the split that directly contains `pane_id`.
    ///
    /// The pane itself is a leaf, so the node to change is its parent. The
    /// child sizes describe the old axis and cannot carry over: a horizontal
    /// node's `first.cols` was carved out of the width, which says nothing
    /// about how many rows the first child should get once the split runs the
    /// other way. apply_sizes_from_splits applies `first`/`second` verbatim
    /// rather than redistributing, so the children have to be resized here to
    /// span the node's full extent along the new axis; leaving the old values
    /// in place collapses one child to nothing and the pane vanishes.
    fn toggle_split_direction_for_pane(&mut self, pane_id: PaneId) -> bool {
        if self.zoomed.is_some() {
            return false;
        }

        let mut cursor = self.pane.take().unwrap().cursor();

        // Walk to the leaf holding this pane, then step up to its split.
        loop {
            if cursor.is_leaf()
                && cursor
                    .leaf_mut()
                    .map(|p| p.pane_id() == pane_id)
                    .unwrap_or(false)
            {
                match cursor.go_up() {
                    Ok(parent) => {
                        cursor = parent;
                        break;
                    }
                    // A lone pane has no split to flip.
                    Err(c) => {
                        self.pane.replace(c.tree());
                        return false;
                    }
                }
            }
            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    return false;
                }
            }
        }

        let cell_dimensions = self.cell_dimensions();
        if let Ok(Some(node)) = cursor.node_mut() {
            // Measure the node along both axes before flipping, while the
            // helpers still agree with the stored direction.
            let total_cols = node.width();
            let total_rows = node.height();

            node.direction = match node.direction {
                SplitDirection::Horizontal => SplitDirection::Vertical,
                SplitDirection::Vertical => SplitDirection::Horizontal,
            };

            // One cell of the extent is the divider, so the children share
            // what is left. An odd remainder goes to the first child.
            match node.direction {
                SplitDirection::Vertical => {
                    let usable = total_rows.saturating_sub(1);
                    let first_rows = (usable + 1) / 2;
                    node.first.rows = first_rows;
                    node.second.rows = usable.saturating_sub(first_rows);
                    node.first.cols = total_cols;
                    node.second.cols = total_cols;
                }
                SplitDirection::Horizontal => {
                    let usable = total_cols.saturating_sub(1);
                    let first_cols = (usable + 1) / 2;
                    node.first.cols = first_cols;
                    node.second.cols = usable.saturating_sub(first_cols);
                    node.first.rows = total_rows;
                    node.second.rows = total_rows;
                }
            }

            for part in [&mut node.first, &mut node.second] {
                part.pixel_width = part.cols.saturating_mul(cell_dimensions.pixel_width);
                part.pixel_height = part.rows.saturating_mul(cell_dimensions.pixel_height);
            }
        } else {
            self.pane.replace(cursor.tree());
            return false;
        }

        // Sizes across the whole tab are recomputed from the split data, the
        // same sequence rotate_clockwise uses after it rearranges the tree.
        self.pane.replace(cursor.tree());
        let size = self.size;
        apply_sizes_from_splits(self.pane.as_mut().unwrap(), &size);
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
        true
    }

    fn swap_panes(&mut self, a: PaneId, b: PaneId) -> bool {
        if a == b || self.zoomed.is_some() || !self.hidden.is_empty() {
            return false;
        }
        // Trading a group member's place would split the group's subtree.
        if matches!(self.groups.unit_of(a), Unit::Group(_))
            || matches!(self.groups.unit_of(b), Unit::Group(_))
        {
            return false;
        }
        let panes: Vec<Arc<dyn Pane>> = self
            .iter_panes_ignoring_zoom()
            .into_iter()
            .map(|p| p.pane)
            .collect();
        let find = |id: PaneId| panes.iter().find(|p| p.pane_id() == id).cloned();
        let (Some(pane_a), Some(pane_b)) = (find(a), find(b)) else {
            return false;
        };
        let active_id = self.get_active_pane().map(|p| p.pane_id());

        let mut cursor = self.pane.take().unwrap().cursor();
        loop {
            if cursor.is_leaf() {
                let leaf = cursor.leaf_mut().unwrap();
                // A replaced leaf is behind the cursor, so it is not visited
                // again and cannot be swapped back.
                if leaf.pane_id() == a {
                    *leaf = Arc::clone(&pane_b);
                } else if leaf.pane_id() == b {
                    *leaf = Arc::clone(&pane_a);
                }
            }
            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    break;
                }
            }
        }

        // The slots keep their sizes; the panes in them are resized to match.
        let size = self.size;
        apply_sizes_from_splits(self.pane.as_mut().unwrap(), &size);
        if let Some(id) = active_id {
            if let Some(p) = self
                .iter_panes_ignoring_zoom()
                .into_iter()
                .find(|p| p.pane.pane_id() == id)
            {
                self.active = p.index;
            }
        }
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
        true
    }

    fn focus_main_fraction(&self) -> Option<f32> {
        if !self.focus_arranged || self.zoomed.is_some() {
            return None;
        }
        // The shape focus_layout builds: one leaf on the left of a
        // left/right split, and only top/bottom splits on the right.
        fn all_stacked(tree: &Tree) -> bool {
            match tree {
                Tree::Empty => false,
                Tree::Leaf(_) => true,
                Tree::Node { left, right, data } => {
                    matches!(data, Some(d) if d.direction == SplitDirection::Vertical)
                        && all_stacked(left)
                        && all_stacked(right)
                }
            }
        }
        match self.pane.as_ref()? {
            Tree::Node {
                left,
                right,
                data: Some(data),
            } if data.direction == SplitDirection::Horizontal
                && matches!(**left, Tree::Leaf(_))
                && all_stacked(right) =>
            {
                let usable = data.first.cols + data.second.cols;
                (usable > 0).then(|| data.first.cols as f32 / usable as f32)
            }
            _ => None,
        }
    }

    /// After a resize, keep the focus layout's main width for next time.
    fn remember_focus_width(&self) {
        if let Some(fraction) = self.focus_main_fraction() {
            Mux::try_get().map(|mux| mux.set_focus_main_fraction(fraction));
        }
    }

    fn focus_layout(&mut self, pane_id: PaneId, main_fraction: f32) -> bool {
        // A hidden pane has no place in a fresh tree that would keep it
        // hidden, so leave such a tab alone rather than reveal it.
        if !self.hidden.is_empty() || self.has_groups() {
            return false;
        }
        self.set_zoomed(false);

        // Already in the focus shape: trade places with the main pane. The
        // others stay where they are, so only the pair moves.
        if self.focus_main_fraction().is_some() {
            let main_id = match self.pane.as_ref() {
                Some(Tree::Node { left, .. }) => match **left {
                    Tree::Leaf(ref p) => Some(p.pane_id()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(main_id) = main_id {
                if main_id == pane_id {
                    return true;
                }
                if self.swap_panes(main_id, pane_id) {
                    // The new main pane is the first leaf in preorder.
                    self.active = 0;
                    self.focus_arranged = true;
                    return true;
                }
            }
        }

        let panes: Vec<Arc<dyn Pane>> = self
            .iter_panes_ignoring_zoom()
            .into_iter()
            .map(|p| p.pane)
            .collect();
        let Some(main_idx) = panes.iter().position(|p| p.pane_id() == pane_id) else {
            return false;
        };
        if panes.len() < 2 {
            return false;
        }
        let mut others = panes.clone();
        let main = others.remove(main_idx);

        // Every stacked pane needs one row, plus one divider row between
        // neighbours; the main pane and the stack need a column each.
        let size = self.size;
        if size.rows < 2 * others.len() - 1 || size.cols < 3 {
            return false;
        }
        let cell = self.cell_dimensions();
        let sized = |cols: usize, rows: usize| TerminalSize {
            rows,
            cols,
            pixel_width: cols.saturating_mul(cell.pixel_width),
            pixel_height: rows.saturating_mul(cell.pixel_height),
            dpi: size.dpi,
        };

        // One column of the width is the divider between main and stack.
        let usable = size.cols - 1;
        let main_cols =
            ((usable as f32 * main_fraction.clamp(0.0, 1.0)).round() as usize).clamp(1, usable - 1);
        let stack_cols = usable - main_cols;

        // Each split hands its first child an equal share of what remains,
        // so the stack ends up evenly divided; a remainder goes to the last.
        fn stack(panes: &[Arc<dyn Pane>], cols: usize, rows: usize, sized: &dyn Fn(usize, usize) -> TerminalSize) -> Tree {
            if panes.len() == 1 {
                return Tree::Leaf(Arc::clone(&panes[0]));
            }
            let n = panes.len();
            let first_rows = ((rows - (n - 1)) / n).max(1);
            let rest_rows = rows - first_rows - 1;
            Tree::Node {
                left: Box::new(Tree::Leaf(Arc::clone(&panes[0]))),
                right: Box::new(stack(&panes[1..], cols, rest_rows, sized)),
                data: Some(SplitDirectionAndSize {
                    direction: SplitDirection::Vertical,
                    first: sized(cols, first_rows),
                    second: sized(cols, rest_rows),
                }),
            }
        }

        let root = Tree::Node {
            left: Box::new(Tree::Leaf(main)),
            right: Box::new(stack(&others, stack_cols, size.rows, &sized)),
            data: Some(SplitDirectionAndSize {
                direction: SplitDirection::Horizontal,
                first: sized(main_cols, size.rows),
                second: sized(stack_cols, size.rows),
            }),
        };

        self.pane.replace(root);
        self.focus_arranged = true;
        // The main pane is the first leaf in preorder, so index 0.
        self.active = 0;
        apply_sizes_from_splits(self.pane.as_mut().unwrap(), &size);
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
        true
    }

    fn adjust_node_at_cursor(&mut self, cursor: &mut Cursor, delta: isize) {
        let cell_dimensions = self.cell_dimensions();
        if let Ok(Some(node)) = cursor.node_mut() {
            match node.direction {
                SplitDirection::Horizontal => {
                    let width = node.width();

                    let mut cols = node.first.cols as isize;
                    cols = cols
                        .saturating_add(delta)
                        .max(1)
                        .min((width as isize).saturating_sub(2));
                    node.first.cols = cols as usize;
                    node.first.pixel_width =
                        node.first.cols.saturating_mul(cell_dimensions.pixel_width);

                    node.second.cols = width.saturating_sub(node.first.cols.saturating_add(1));
                    node.second.pixel_width =
                        node.second.cols.saturating_mul(cell_dimensions.pixel_width);
                }
                SplitDirection::Vertical => {
                    let height = node.height();

                    let mut rows = node.first.rows as isize;
                    rows = rows
                        .saturating_add(delta)
                        .max(1)
                        .min((height as isize).saturating_sub(2));
                    node.first.rows = rows as usize;
                    node.first.pixel_height =
                        node.first.rows.saturating_mul(cell_dimensions.pixel_height);

                    node.second.rows = height.saturating_sub(node.first.rows.saturating_add(1));
                    node.second.pixel_height = node
                        .second
                        .rows
                        .saturating_mul(cell_dimensions.pixel_height);
                }
            }
        }
    }

    fn cascade_size_from_cursor(&mut self, mut cursor: Cursor) {
        // Now we need to cascade this down to children
        match cursor.preorder_next() {
            Ok(c) => cursor = c,
            Err(c) => {
                self.pane.replace(c.tree());
                return;
            }
        }
        let root_size = self.size;

        loop {
            // Figure out the available size by looking at our immediate parent node.
            // If we are the root, look at the provided new size
            let pane_size = if let Some((branch, Some(parent))) = cursor.path_to_root().next() {
                if branch == PathBranch::IsRight {
                    parent.second
                } else {
                    parent.first
                }
            } else {
                root_size
            };

            if cursor.is_leaf() {
                // Apply our size to the tty
                cursor.leaf_mut().map(|pane| pane.resize(pane_size));
            } else {
                self.apply_pane_size(pane_size, &mut cursor);
            }
            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    break;
                }
            }
        }
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    fn adjust_pane_size(&mut self, direction: PaneDirection, amount: usize) {
        if self.zoomed.is_some() {
            return;
        }
        let active_index = self.active;
        let mut cursor = self.pane.take().unwrap().cursor();
        let mut index = 0;

        // Position cursor on the active leaf
        loop {
            if cursor.is_leaf() {
                if index == active_index {
                    // Found it
                    break;
                }
                index += 1;
            }
            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    // Didn't find it
                    self.pane.replace(c.tree());
                    return;
                }
            }
        }

        // We are on the active leaf.
        // Now we go up until we find the parent node that is
        // aligned with the desired direction.
        let split_direction = match direction {
            PaneDirection::Left | PaneDirection::Right => SplitDirection::Horizontal,
            PaneDirection::Up | PaneDirection::Down => SplitDirection::Vertical,
            PaneDirection::Next | PaneDirection::Prev => unreachable!(),
        };
        let delta = match direction {
            PaneDirection::Down | PaneDirection::Right => amount as isize,
            PaneDirection::Up | PaneDirection::Left => -(amount as isize),
            PaneDirection::Next | PaneDirection::Prev => unreachable!(),
        };
        loop {
            match cursor.go_up() {
                Ok(mut c) => {
                    if let Ok(Some(node)) = c.node_mut() {
                        if node.direction == split_direction {
                            self.adjust_node_at_cursor(&mut c, delta);
                            self.cascade_size_from_cursor(c);
                            return;
                        }
                    }

                    cursor = c;
                }

                Err(c) => {
                    self.pane.replace(c.tree());
                    return;
                }
            }
        }
    }

    fn activate_pane_direction(&mut self, direction: PaneDirection) {
        if self.zoomed.is_some() {
            if !configuration().unzoom_on_switch_pane {
                return;
            }
            self.toggle_zoom();
        }
        if let Some(panel_idx) = self.get_pane_direction(direction, false) {
            self.set_active_idx(panel_idx);
        }
        if let Some(mux) = Mux::try_get() {
            if let Some(window_id) = mux.window_containing_tab(self.id) {
                mux.notify(MuxNotification::WindowInvalidated(window_id));
            }
        }
    }

    fn get_pane_direction(&mut self, direction: PaneDirection, ignore_zoom: bool) -> Option<usize> {
        let panes = if ignore_zoom {
            self.iter_panes_ignoring_zoom()
        } else {
            self.iter_panes()
        };

        let active = match panes.iter().find(|pane| pane.is_active) {
            Some(p) => p,
            None => {
                // No active pane somehow...
                return Some(0);
            }
        };

        if matches!(direction, PaneDirection::Next | PaneDirection::Prev) {
            // Step through the panes that are on show; indices may skip
            // hidden panes.
            let position = panes.iter().position(|p| p.index == active.index).unwrap_or(0);
            let count = panes.len();
            let target = if direction == PaneDirection::Next {
                (position + 1) % count
            } else {
                (position + count - 1) % count
            };
            return Some(panes[target].index);
        }

        let mut best = None;

        let recency = &self.recency;

        fn edge_intersects(
            active_start: usize,
            active_size: usize,
            current_start: usize,
            current_size: usize,
        ) -> bool {
            intersects_range(
                &(active_start..active_start + active_size),
                &(current_start..current_start + current_size),
            )
        }

        for pane in &panes {
            let score = match direction {
                PaneDirection::Right => {
                    if pane.left == active.left + active.width + 1
                        && edge_intersects(active.top, active.height, pane.top, pane.height)
                    {
                        1 + recency.score(pane.index)
                    } else {
                        0
                    }
                }
                PaneDirection::Left => {
                    if pane.left + pane.width + 1 == active.left
                        && edge_intersects(active.top, active.height, pane.top, pane.height)
                    {
                        1 + recency.score(pane.index)
                    } else {
                        0
                    }
                }
                PaneDirection::Up => {
                    if pane.top + pane.height + 1 == active.top
                        && edge_intersects(active.left, active.width, pane.left, pane.width)
                    {
                        1 + recency.score(pane.index)
                    } else {
                        0
                    }
                }
                PaneDirection::Down => {
                    if active.top + active.height + 1 == pane.top
                        && edge_intersects(active.left, active.width, pane.left, pane.width)
                    {
                        1 + recency.score(pane.index)
                    } else {
                        0
                    }
                }
                PaneDirection::Next | PaneDirection::Prev => unreachable!(),
            };

            if score > 0 {
                let target = match best.take() {
                    Some((best_score, best_pane)) if best_score > score => (best_score, best_pane),
                    _ => (score, pane),
                };
                best.replace(target);
            }
        }

        if let Some((_, target)) = best.take() {
            return Some(target.index);
        }
        None
    }

    fn prune_dead_panes(&mut self) -> bool {
        let mux = Mux::get();
        !self
            .remove_pane_if(
                |_, pane| {
                    // If the pane is no longer known to the mux, then its liveness
                    // state isn't guaranteed to be monitored or updated, so let's
                    // consider the pane effectively dead if it isn't in the mux.
                    // <https://github.com/wez/wezterm/issues/4030>
                    let in_mux = mux.get_pane(pane.pane_id()).is_some();
                    let dead = pane.is_dead();
                    log::trace!(
                        "prune_dead_panes: pane_id={} dead={} in_mux={}",
                        pane.pane_id(),
                        dead,
                        in_mux
                    );
                    dead || !in_mux
                },
                true,
            )
            .is_empty()
    }

    fn kill_pane(&mut self, pane_id: PaneId) -> bool {
        !self
            .remove_pane_if(|_, pane| pane.pane_id() == pane_id, true)
            .is_empty()
    }

    fn kill_panes_in_domain(&mut self, domain: DomainId) -> bool {
        !self
            .remove_pane_if(|_, pane| pane.domain_id() == domain, true)
            .is_empty()
    }

    fn remove_pane(&mut self, pane_id: PaneId) -> Option<Arc<dyn Pane>> {
        let panes = self.remove_pane_if(|_, pane| pane.pane_id() == pane_id, false);
        for pane in panes {
            return Some(pane);
        }
        None
    }

    fn remove_pane_if<F>(&mut self, f: F, kill: bool) -> Vec<Arc<dyn Pane>>
    where
        F: Fn(usize, &Arc<dyn Pane>) -> bool,
    {
        let mut dead_panes = vec![];
        let zoomed_pane = self.zoomed.as_ref().map(|p| p.pane_id());

        {
            let root_size = self.size;
            let mut cursor = self.pane.take().unwrap().cursor();
            let mut pane_index = 0;
            let mut removed_indices = vec![];
            let cell_dims = self.cell_dimensions();

            loop {
                // Figure out the available size by looking at our immediate parent node.
                // If we are the root, look at the tab size
                let pane_size = if let Some((branch, Some(parent))) = cursor.path_to_root().next() {
                    if branch == PathBranch::IsRight {
                        parent.second
                    } else {
                        parent.first
                    }
                } else {
                    root_size
                };

                if cursor.is_leaf() {
                    let pane = Arc::clone(cursor.leaf_mut().unwrap());
                    if f(pane_index, &pane) {
                        removed_indices.push(pane_index);
                        if Some(pane.pane_id()) == zoomed_pane {
                            // If we removed the zoomed pane, un-zoom our state!
                            self.zoomed.take();
                        }
                        let parent;
                        match cursor.unsplit_leaf() {
                            Ok((c, dead, p)) => {
                                dead_panes.push(dead);
                                parent = p.unwrap();
                                cursor = c;
                            }
                            Err(c) => {
                                // We might be the root, for example
                                if c.is_top() && c.is_leaf() {
                                    self.pane.replace(Tree::Empty);
                                    dead_panes.push(pane);
                                } else {
                                    self.pane.replace(c.tree());
                                }
                                break;
                            }
                        };

                        // Now we need to increase the size of the current node
                        // and propagate the revised size to its children.
                        let size = TerminalSize {
                            rows: parent.height(),
                            cols: parent.width(),
                            pixel_width: cell_dims.pixel_width * parent.width(),
                            pixel_height: cell_dims.pixel_height * parent.height(),
                            dpi: cell_dims.dpi,
                        };

                        if let Some(unsplit) = cursor.leaf_mut() {
                            unsplit.resize(size).ok();
                        } else {
                            self.apply_pane_size(size, &mut cursor);
                        }
                    } else if !dead_panes.is_empty() {
                        // Apply our revised size to the tty
                        pane.resize(pane_size).ok();
                    }

                    pane_index += 1;
                } else if !dead_panes.is_empty() {
                    self.apply_pane_size(pane_size, &mut cursor);
                }
                match cursor.preorder_next() {
                    Ok(c) => cursor = c,
                    Err(c) => {
                        self.pane.replace(c.tree());
                        break;
                    }
                }
            }

            // Figure out which pane should now be active.
            // If panes earlier than the active pane were closed, then we
            // need to shift the active pane down
            let active_idx = self.active;
            removed_indices.retain(|&idx| idx <= active_idx);
            self.active = active_idx.saturating_sub(removed_indices.len());
        }

        if !self.docking && !dead_panes.is_empty() {
            let removed: Vec<PaneId> = dead_panes.iter().map(|p| p.pane_id()).collect();
            self.prune_removed_panes(&removed);
        }

        if !dead_panes.is_empty() && kill {
            let to_kill: Vec<_> = dead_panes.iter().map(|p| p.pane_id()).collect();
            promise::spawn::spawn_into_main_thread(async move {
                let mux = Mux::get();
                for pane_id in to_kill.into_iter() {
                    mux.remove_pane(pane_id);
                }
            })
            .detach();
        }
        dead_panes
    }

    fn can_close_without_prompting(&mut self, reason: CloseReason) -> bool {
        let panes = self.iter_panes_ignoring_zoom();
        for pos in &panes {
            if !pos.pane.can_close_without_prompting(reason) {
                return false;
            }
        }
        true
    }

    fn is_dead(&mut self) -> bool {
        // Make sure we account for all panes, so that we don't
        // kill the whole tab if the zoomed pane is dead!
        let panes = self.iter_panes_ignoring_zoom();
        let mut dead_count = 0;
        for pos in &panes {
            if pos.pane.is_dead() {
                dead_count += 1;
            }
        }
        dead_count == panes.len()
    }

    fn get_active_pane(&mut self) -> Option<Arc<dyn Pane>> {
        if let Some(zoomed) = self.zoomed.as_ref() {
            return Some(Arc::clone(zoomed));
        }

        self.iter_panes_ignoring_zoom()
            .iter()
            .nth(self.active)
            .map(|p| Arc::clone(&p.pane))
    }

    fn get_active_idx(&self) -> usize {
        self.active
    }

    fn set_active_pane(&mut self, pane: &Arc<dyn Pane>) {
        let prior = self.get_active_pane();

        if is_pane(pane, &prior.as_ref()) {
            return;
        }

        if self.zoomed.is_some() {
            if !configuration().unzoom_on_switch_pane {
                return;
            }
            self.toggle_zoom();
        }

        self.reveal_stacked_pane(pane.pane_id());
        if let Some(item) = self
            .iter_panes_ignoring_zoom()
            .iter()
            .find(|p| p.pane.pane_id() == pane.pane_id())
        {
            self.active = item.index;
            self.recency.tag(item.index);
            self.groups.note_active(pane.pane_id());
            self.advise_focus_change(prior);
        }
    }

    fn advise_focus_change(&mut self, prior: Option<Arc<dyn Pane>>) {
        // No Mux exists in unit tests; focus still has to reach the panes.
        let mux = Mux::try_get();
        let notify_focused = |pane_id: PaneId| {
            if let Some(mux) = &mux {
                mux.notify(MuxNotification::PaneFocused(pane_id));
            }
        };
        let current = self.get_active_pane();
        match (prior, current) {
            (Some(prior), Some(current)) if prior.pane_id() != current.pane_id() => {
                prior.focus_changed(false);
                current.focus_changed(true);
                notify_focused(current.pane_id());
            }
            (None, Some(current)) => {
                current.focus_changed(true);
                notify_focused(current.pane_id());
            }
            (Some(prior), None) => {
                prior.focus_changed(false);
            }
            (Some(_), Some(_)) | (None, None) => {
                // no change
            }
        }
    }

    fn set_active_idx(&mut self, pane_index: usize) {
        let prior = self.get_active_pane();
        let target = self
            .iter_panes_ignoring_zoom()
            .into_iter()
            .find(|p| p.index == pane_index)
            .map(|p| p.pane.pane_id());
        if let Some(id) = target {
            self.reveal_stacked_pane(id);
        }
        self.active = pane_index;
        self.recency.tag(pane_index);
        if let Some(id) = self.get_active_pane().map(|p| p.pane_id()) {
            self.groups.note_active(id);
        }
        self.advise_focus_change(prior);
    }

    fn assign_pane(&mut self, pane: &Arc<dyn Pane>) {
        match Tree::new().cursor().assign_top(Arc::clone(pane)) {
            Ok(c) => self.pane = Some(c.tree()),
            Err(_) => panic!("tried to assign root pane to non-empty tree"),
        }
    }

    fn cell_dimensions(&self) -> TerminalSize {
        cell_dimensions(&self.size)
    }

    fn swap_active_with_index(&mut self, pane_index: usize, keep_focus: bool) -> Option<()> {
        let active_idx = self.get_active_idx();
        let mut pane = self.get_active_pane()?;
        log::trace!(
            "swap_active_with_index: pane_index {} active {}",
            pane_index,
            active_idx
        );

        {
            let mut cursor = self.pane.take().unwrap().cursor();

            // locate the requested index
            match cursor.go_to_nth_leaf(pane_index) {
                Ok(c) => cursor = c,
                Err(c) => {
                    log::trace!("didn't find pane {pane_index}");
                    self.pane.replace(c.tree());
                    return None;
                }
            };

            std::mem::swap(&mut pane, cursor.leaf_mut().unwrap());

            // re-position to the root
            cursor = cursor.tree().cursor();

            // and now go and update the active idx
            match cursor.go_to_nth_leaf(active_idx) {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    log::trace!("didn't find active {active_idx}");
                    return None;
                }
            };

            std::mem::swap(&mut pane, cursor.leaf_mut().unwrap());
            self.pane.replace(cursor.tree());

            // Advise the panes of their new sizes
            let size = self.size;
            apply_sizes_from_splits(self.pane.as_mut().unwrap(), &size);
        }

        // And update focus
        if keep_focus {
            self.set_active_idx(pane_index);
        } else {
            self.advise_focus_change(Some(pane));
        }
        None
    }

    fn compute_split_size(
        &mut self,
        pane_index: usize,
        request: SplitRequest,
    ) -> Option<SplitDirectionAndSize> {
        let cell_dims = self.cell_dimensions();

        if request.top_level {
            let size = self.size;

            let ((width1, width2), (height1, height2)) = match request.direction {
                SplitDirection::Horizontal => (
                    split_dimension(size.cols as usize, request),
                    (size.rows as usize, size.rows as usize),
                ),
                SplitDirection::Vertical => (
                    (size.cols as usize, size.cols as usize),
                    split_dimension(size.rows as usize, request),
                ),
            };

            return Some(SplitDirectionAndSize {
                direction: request.direction,
                first: TerminalSize {
                    rows: height1 as _,
                    cols: width1 as _,
                    pixel_height: cell_dims.pixel_height * height1,
                    pixel_width: cell_dims.pixel_width * width1,
                    dpi: cell_dims.dpi,
                },
                second: TerminalSize {
                    rows: height2 as _,
                    cols: width2 as _,
                    pixel_height: cell_dims.pixel_height * height2,
                    pixel_width: cell_dims.pixel_width * width2,
                    dpi: cell_dims.dpi,
                },
            });
        }

        // Ensure that we're not zoomed, otherwise we'll end up in
        // a bogus split state (https://github.com/wez/wezterm/issues/723)
        self.set_zoomed(false);

        // `pane_index` counts every pane, hidden ones included.
        self.iter_panes()
            .into_iter()
            .find(|pos| pos.index == pane_index)
            .map(|pos| split_size_for_extent(&cell_dims, pos.width, pos.height, request))
    }

    fn split_and_insert(
        &mut self,
        pane_index: usize,
        request: SplitRequest,
        pane: Arc<dyn Pane>,
    ) -> anyhow::Result<usize> {
        if self.zoomed.is_some() {
            anyhow::bail!("cannot split while zoomed");
        }
        // A new pane makes this some other layout, even when it happens to
        // keep focus_layout's shape.
        self.focus_arranged = false;

        let split_ids: (PaneId, PaneId);
        {
            let split_info = self
                .compute_split_size(pane_index, request)
                .ok_or_else(|| {
                    anyhow::anyhow!("invalid pane_index {}; cannot split!", pane_index)
                })?;

            let tab_size = self.size;
            if split_info.first.rows == 0
                || split_info.first.cols == 0
                || split_info.second.rows == 0
                || split_info.second.cols == 0
                || split_info.top_of_second() + split_info.second.rows > tab_size.rows
                || split_info.left_of_second() + split_info.second.cols > tab_size.cols
            {
                log::error!(
                    "No space for split!!! {:#?} height={} width={} top_of_second={} left_of_second={} tab_size={:?}",
                    split_info,
                    split_info.height(),
                    split_info.width(),
                    split_info.top_of_second(),
                    split_info.left_of_second(),
                    tab_size
                );
                anyhow::bail!("No space for split!");
            }

            let needs_resize = if request.top_level {
                self.pane.as_ref().unwrap().num_leaves() > 1
            } else {
                false
            };

            if needs_resize {
                // Pre-emptively resize the tab contents down to
                // match the target size; it's easier to reuse
                // existing resize logic that way
                if request.target_is_second {
                    self.resize(split_info.first.clone());
                } else {
                    self.resize(split_info.second.clone());
                }
            }

            let mut cursor = self.pane.take().unwrap().cursor();

            if request.top_level && !cursor.is_leaf() {
                let result = if request.target_is_second {
                    cursor.split_node_and_insert_right(Arc::clone(&pane))
                } else {
                    cursor.split_node_and_insert_left(Arc::clone(&pane))
                };
                cursor = match result {
                    Ok(c) => {
                        cursor = match c.assign_node(Some(split_info)) {
                            Err(c) | Ok(c) => c,
                        };

                        self.pane.replace(cursor.tree());

                        let pane_index = if request.target_is_second {
                            self.pane.as_ref().unwrap().num_leaves().saturating_sub(1)
                        } else {
                            0
                        };

                        self.active = pane_index;
                        self.recency.tag(pane_index);
                        return Ok(pane_index);
                    }
                    Err(cursor) => cursor,
                };
            }

            match cursor.go_to_nth_leaf(pane_index) {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    anyhow::bail!("invalid pane_index {}; cannot split!", pane_index);
                }
            };

            let existing_pane = Arc::clone(cursor.leaf_mut().unwrap());
            split_ids = (existing_pane.pane_id(), pane.pane_id());

            let (pane1, pane2) = if request.target_is_second {
                (existing_pane, pane)
            } else {
                (pane, existing_pane)
            };

            pane1.resize(split_info.first)?;
            pane2.resize(split_info.second.clone())?;

            *cursor.leaf_mut().unwrap() = pane1;

            match cursor.split_leaf_and_insert_right(pane2) {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    anyhow::bail!("invalid pane_index {}; cannot split!", pane_index);
                }
            };

            // cursor now points to the newly created split node;
            // we need to populate its split information
            match cursor.assign_node(Some(split_info)) {
                Err(c) | Ok(c) => self.pane.replace(c.tree()),
            };

            if request.target_is_second {
                self.active = pane_index + 1;
                self.recency.tag(pane_index + 1);
            }
        }

        self.groups.adopt(split_ids.0, split_ids.1);

        log::debug!("split info after split: {:#?}", self.iter_splits());
        log::debug!("pane info after split: {:#?}", self.iter_panes());

        Ok(if request.target_is_second {
            pane_index + 1
        } else {
            pane_index
        })
    }

    fn get_zoomed_pane(&self) -> Option<Arc<dyn Pane>> {
        self.zoomed.clone()
    }
}

/// This type is used directly by the codec, take care to bump
/// the codec version if you change this
#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub enum PaneNode {
    Empty,
    Split {
        left: Box<PaneNode>,
        right: Box<PaneNode>,
        node: SplitDirectionAndSize,
    },
    Leaf(PaneEntry),
}

impl PaneNode {
    pub fn into_tree(self) -> bintree::Tree<PaneEntry, SplitDirectionAndSize> {
        match self {
            PaneNode::Empty => bintree::Tree::Empty,
            PaneNode::Split { left, right, node } => bintree::Tree::Node {
                left: Box::new((*left).into_tree()),
                right: Box::new((*right).into_tree()),
                data: Some(node),
            },
            PaneNode::Leaf(e) => bintree::Tree::Leaf(e),
        }
    }

    pub fn root_size(&self) -> Option<TerminalSize> {
        match self {
            PaneNode::Empty => None,
            PaneNode::Split { node, .. } => Some(node.size()),
            PaneNode::Leaf(entry) => Some(entry.size),
        }
    }

    pub fn window_and_tab_ids(&self) -> Option<(WindowId, TabId)> {
        match self {
            PaneNode::Empty => None,
            PaneNode::Split { left, right, .. } => match left.window_and_tab_ids() {
                Some(res) => Some(res),
                None => right.window_and_tab_ids(),
            },
            PaneNode::Leaf(entry) => Some((entry.window_id, entry.tab_id)),
        }
    }
}

/// This type is used directly by the codec, take care to bump
/// the codec version if you change this
#[derive(Deserialize, Serialize, PartialEq, Debug, Clone)]
pub struct PaneEntry {
    pub window_id: WindowId,
    pub tab_id: TabId,
    pub pane_id: PaneId,
    pub title: String,
    pub size: TerminalSize,
    pub working_dir: Option<SerdeUrl>,
    pub is_active_pane: bool,
    pub is_zoomed_pane: bool,
    pub workspace: String,
    pub cursor_pos: StableCursorPosition,
    pub physical_top: StableRowIndex,
    pub top_row: usize,
    pub left_col: usize,
    pub tty_name: Option<String>,
}

#[derive(Deserialize, Clone, Serialize, PartialEq, Debug)]
#[serde(try_from = "String", into = "String")]
pub struct SerdeUrl {
    pub url: Url,
}

impl std::convert::TryFrom<String> for SerdeUrl {
    type Error = url::ParseError;
    fn try_from(s: String) -> Result<SerdeUrl, url::ParseError> {
        let url = Url::parse(&s)?;
        Ok(SerdeUrl { url })
    }
}

impl From<Url> for SerdeUrl {
    fn from(url: Url) -> SerdeUrl {
        SerdeUrl { url }
    }
}

impl Into<Url> for SerdeUrl {
    fn into(self) -> Url {
        self.url
    }
}

impl Into<String> for SerdeUrl {
    fn into(self) -> String {
        self.url.as_str().into()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::renderable::*;
    use parking_lot::{MappedMutexGuard, Mutex};
    use rangeset::RangeSet;
    use std::ops::Range;
    use termwiz::surface::SequenceNo;
    use url::Url;
    use terminaler_term::color::ColorPalette;
    use terminaler_term::{KeyCode, KeyModifiers, Line, MouseEvent, StableRowIndex};

    struct FakePane {
        id: PaneId,
        size: Mutex<TerminalSize>,
    }

    impl FakePane {
        fn new(id: PaneId, size: TerminalSize) -> Arc<dyn Pane> {
            Arc::new(Self {
                id,
                size: Mutex::new(size),
            })
        }
    }

    impl Pane for FakePane {
        fn pane_id(&self) -> PaneId {
            self.id
        }

        fn get_cursor_position(&self) -> StableCursorPosition {
            unimplemented!();
        }

        fn get_current_seqno(&self) -> SequenceNo {
            unimplemented!();
        }

        fn get_changed_since(
            &self,
            _lines: Range<StableRowIndex>,
            _: SequenceNo,
        ) -> RangeSet<StableRowIndex> {
            unimplemented!();
        }

        fn with_lines_mut(
            &self,
            _stable_range: Range<StableRowIndex>,
            _with_lines: &mut dyn WithPaneLines,
        ) {
            unimplemented!();
        }

        fn for_each_logical_line_in_stable_range_mut(
            &self,
            _lines: Range<StableRowIndex>,
            _for_line: &mut dyn ForEachPaneLogicalLine,
        ) {
            unimplemented!();
        }

        fn get_lines(&self, _lines: Range<StableRowIndex>) -> (StableRowIndex, Vec<Line>) {
            unimplemented!();
        }

        fn get_logical_lines(&self, _lines: Range<StableRowIndex>) -> Vec<LogicalLine> {
            unimplemented!();
        }

        fn get_dimensions(&self) -> RenderableDimensions {
            unimplemented!();
        }

        fn get_title(&self) -> String {
            unimplemented!()
        }
        fn send_paste(&self, _text: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        fn reader(&self) -> anyhow::Result<Option<Box<dyn std::io::Read + Send>>> {
            Ok(None)
        }
        fn writer(&self) -> MappedMutexGuard<dyn std::io::Write> {
            unimplemented!()
        }
        fn resize(&self, size: TerminalSize) -> anyhow::Result<()> {
            *self.size.lock() = size;
            Ok(())
        }

        fn key_down(&self, _key: KeyCode, _mods: KeyModifiers) -> anyhow::Result<()> {
            unimplemented!()
        }
        fn key_up(&self, _: KeyCode, _: KeyModifiers) -> anyhow::Result<()> {
            unimplemented!()
        }
        fn mouse_event(&self, _event: MouseEvent) -> anyhow::Result<()> {
            unimplemented!()
        }
        fn is_dead(&self) -> bool {
            false
        }
        fn palette(&self) -> ColorPalette {
            unimplemented!()
        }
        fn domain_id(&self) -> DomainId {
            1
        }
        fn is_mouse_grabbed(&self) -> bool {
            false
        }
        fn is_alt_screen_active(&self) -> bool {
            false
        }
        fn get_current_working_dir(&self, _policy: CachePolicy) -> Option<Url> {
            None
        }
    }

    #[test]
    fn toggle_split_direction_keeps_both_panes_visible() {
        let size = TerminalSize {
            rows: 24,
            cols: 80,
            pixel_width: 800,
            pixel_height: 600,
            dpi: 96,
        };

        let tab = Tab::new(&size);
        tab.assign_pane(&FakePane::new(1, size));

        let horz_size = tab
            .compute_split_size(
                0,
                SplitRequest {
                    direction: SplitDirection::Horizontal,
                    ..Default::default()
                },
            )
            .unwrap();
        tab.split_and_insert(
            0,
            SplitRequest {
                direction: SplitDirection::Horizontal,
                ..Default::default()
            },
            FakePane::new(2, horz_size.second),
        )
        .unwrap();

        // Side by side to begin with: same top, different left.
        let panes = tab.iter_panes();
        assert_eq!(2, panes.len());
        assert_eq!(panes[0].top, panes[1].top);
        assert_ne!(panes[0].left, panes[1].left);

        assert!(tab.toggle_split_direction_for_pane(1));

        // Now stacked, and crucially both panes still occupy real space —
        // applying the pre-flip sizes to the new axis used to collapse one of
        // them to nothing, which read as the pane disappearing.
        let panes = tab.iter_panes();
        assert_eq!(2, panes.len());
        assert_eq!(panes[0].left, panes[1].left);
        assert_ne!(panes[0].top, panes[1].top);
        for p in &panes {
            assert!(p.height > 0, "pane {} collapsed: {:?}", p.index, p);
            assert!(p.width > 0, "pane {} collapsed: {:?}", p.index, p);
        }

        // And flipping back restores a side-by-side arrangement.
        assert!(tab.toggle_split_direction_for_pane(1));
        let panes = tab.iter_panes();
        assert_eq!(panes[0].top, panes[1].top);
        assert_ne!(panes[0].left, panes[1].left);
        for p in &panes {
            assert!(p.height > 0 && p.width > 0, "pane {} collapsed", p.index);
        }
    }

    #[test]
    fn focus_layout_puts_the_pane_left_and_stacks_the_rest() {
        let size = TerminalSize {
            rows: 24,
            cols: 80,
            pixel_width: 800,
            pixel_height: 600,
            dpi: 96,
        };
        let tab = Tab::new(&size);
        tab.assign_pane(&FakePane::new(1, size));
        // A lone pane has nothing to stack.
        assert!(!tab.focus_layout(1, 0.6));

        // Three panes side by side: 1 | 2 | 3.
        for (idx, id) in [(0, 2), (1, 3)] {
            let req = SplitRequest {
                direction: SplitDirection::Horizontal,
                ..Default::default()
            };
            let split = tab.compute_split_size(idx, req).unwrap();
            tab.split_and_insert(idx, req, FakePane::new(id, split.second))
                .unwrap();
        }

        // Focus the middle pane.
        assert!(tab.focus_layout(2, 0.6));
        let panes = tab.iter_panes();
        let ids: Vec<PaneId> = panes.iter().map(|p| p.pane.pane_id()).collect();
        assert_eq!(ids, vec![2, 1, 3], "main first, then the others in order");

        let (main, stack) = (&panes[0], &panes[1..]);
        assert!(main.is_active);
        assert_eq!((main.left, main.top, main.height), (0, 0, 24));
        assert_eq!(main.width, 79 * 60 / 100);
        for p in stack {
            assert_eq!(p.left, main.width + 1, "stack sits right of the divider");
            assert_eq!(p.width, 80 - main.width - 1);
            assert!(p.height > 0);
        }
        assert_eq!(stack[0].top, 0);
        assert_eq!(stack[1].top, stack[0].height + 1, "stacked top to bottom");
        assert_eq!(stack[0].height + 1 + stack[1].height, 24, "stack fills the height");

        // An unknown pane leaves the layout alone.
        assert!(!tab.focus_layout(99, 0.6));
        assert_eq!(tab.iter_panes()[0].pane.pane_id(), 2);

        // Focusing another pane while already in the focus shape trades
        // places with the main pane; the rest of the stack stays put.
        assert!(tab.focus_layout(3, 0.6));
        let after = tab.iter_panes();
        let ids: Vec<PaneId> = after.iter().map(|p| p.pane.pane_id()).collect();
        assert_eq!(ids, vec![3, 1, 2], "3 is main, 2 took 3's old seat, 1 did not move");
        assert!(after[0].is_active);
        assert_eq!((after[1].top, after[1].height), (stack[0].top, stack[0].height));
        assert_eq!((after[2].top, after[2].height), (stack[1].top, stack[1].height));
    }

    #[test]
    fn focus_layout_reports_the_width_the_user_resized_it_to() {
        let (tab, _) = three_across();
        // Not a focus arrangement yet, even though it is a left/right split.
        assert_eq!(tab.focus_main_fraction(), None);

        assert!(tab.focus_layout(1, 0.6));
        let main_cols = tab.iter_panes()[0].width;
        assert_eq!(tab.focus_main_fraction(), Some(main_cols as f32 / 79.0));

        // Drag the main divider (the root split, index 0) 10 columns right,
        // as a user widening the main pane and shrinking the stack would.
        tab.resize_split_by(0, 10);
        let widened = tab.iter_panes()[0].width;
        assert_eq!(widened, main_cols + 10);
        let fraction = tab.focus_main_fraction().unwrap();
        assert_eq!(fraction, widened as f32 / 79.0);

        // Re-focusing another pane with that fraction keeps the width.
        assert!(tab.focus_layout(3, fraction));
        assert_eq!(tab.iter_panes()[0].pane.pane_id(), 3);
        assert_eq!(tab.iter_panes()[0].width, widened);

        // Swapping keeps the shape, so it is still a focus arrangement.
        assert!(tab.swap_panes(3, 1));
        assert!(tab.focus_main_fraction().is_some());

        // Splitting in a new pane makes it some other layout.
        let idx = tab.iter_panes()[0].index;
        let req = SplitRequest {
            direction: SplitDirection::Vertical,
            ..Default::default()
        };
        let split = tab.compute_split_size(idx, req).unwrap();
        tab.split_and_insert(idx, req, FakePane::new(4, split.second))
            .unwrap();
        assert_eq!(tab.focus_main_fraction(), None);
    }

    /// Three panes side by side, ids 1 | 2 | 3.
    fn three_across() -> (Tab, TerminalSize) {
        let size = TerminalSize {
            rows: 24,
            cols: 80,
            pixel_width: 800,
            pixel_height: 600,
            dpi: 96,
        };
        let tab = Tab::new(&size);
        tab.assign_pane(&FakePane::new(1, size));
        for (idx, id) in [(0, 2), (1, 3)] {
            let req = SplitRequest {
                direction: SplitDirection::Horizontal,
                ..Default::default()
            };
            let split = tab.compute_split_size(idx, req).unwrap();
            tab.split_and_insert(idx, req, FakePane::new(id, split.second))
                .unwrap();
        }
        (tab, size)
    }

    fn ids(tab: &Tab) -> Vec<PaneId> {
        tab.iter_panes().iter().map(|p| p.pane.pane_id()).collect()
    }

    #[test]
    fn swap_panes_trades_places_and_keeps_the_layout_shape() {
        let (tab, _) = three_across();
        let before: Vec<(usize, usize, usize, usize)> = tab
            .iter_panes()
            .iter()
            .map(|p| (p.left, p.top, p.width, p.height))
            .collect();
        let active_id = |tab: &Tab| -> Vec<PaneId> {
            tab.iter_panes()
                .iter()
                .filter(|p| p.is_active)
                .map(|p| p.pane.pane_id())
                .collect()
        };
        // Pane 3 is the active one: the last split made it so. (Moving focus
        // with set_active_idx needs a live Mux, which unit tests do not have.)
        assert_eq!(active_id(&tab), vec![3]);

        assert!(tab.swap_panes(1, 3));
        assert_eq!(ids(&tab), vec![3, 2, 1], "outer panes traded places");
        let after: Vec<(usize, usize, usize, usize)> = tab
            .iter_panes()
            .iter()
            .map(|p| (p.left, p.top, p.width, p.height))
            .collect();
        assert_eq!(before, after, "the slots did not move or change size");
        // Pane 3 was active and still is, now in the first slot.
        assert_eq!(active_id(&tab), vec![3]);
        assert!(tab.iter_panes()[0].is_active);

        // Swapping back restores the original order.
        assert!(tab.swap_panes(3, 1));
        assert_eq!(ids(&tab), vec![1, 2, 3]);

        // Nothing to do: same pane, or a pane that is not in the tab.
        assert!(!tab.swap_panes(2, 2));
        assert!(!tab.swap_panes(2, 99));
        assert_eq!(ids(&tab), vec![1, 2, 3]);
    }

    fn set(ids: &[PaneId]) -> HashSet<PaneId> {
        ids.iter().copied().collect()
    }

    fn leaf_sets(tab: &Tab) -> Vec<HashSet<PaneId>> {
        let inner = tab.inner.lock();
        subtree_leaf_sets(inner.pane.as_ref().unwrap())
    }

    /// Panes 1 | 2 | 3 as the tree (1,(2,3)), then 4 split off pane 1:
    /// ((1,4),(2,3)).
    fn four_panes() -> Tab {
        let (tab, _) = three_across();
        let req = SplitRequest {
            direction: SplitDirection::Vertical,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, req).unwrap();
        tab.split_and_insert(0, req, FakePane::new(4, split.second))
            .unwrap();
        tab
    }

    fn dock_right() -> SplitRequest {
        SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        }
    }

    #[test]
    fn dock_unit_places_two_panes_beside_a_single_pane() {
        let (tab, _) = three_across();
        assert!(tab.dock_unit(&[2, 3], &[1], dock_right()));
        let sets = leaf_sets(&tab);
        assert!(sets.contains(&set(&[1])));
        assert!(sets.contains(&set(&[2, 3])));
        assert!(sets.contains(&set(&[1, 2, 3])));
    }

    #[test]
    fn dock_unit_wraps_a_multi_pane_target_without_splitting_it() {
        let tab = four_panes();
        assert!(tab.dock_unit(&[4], &[2, 3], dock_right()));
        let sets = leaf_sets(&tab);
        assert!(sets.contains(&set(&[2, 3])), "target subtree stays whole");
        assert!(sets.contains(&set(&[2, 3, 4])), "joined under one parent");
        assert!(!sets.contains(&set(&[3, 4])));
    }

    #[test]
    fn dock_unit_keeps_all_panes_alive_and_sized() {
        let tab = four_panes();
        assert!(tab.dock_unit(&[4], &[2, 3], dock_right()));
        let panes = tab.iter_panes();
        let mut found: Vec<PaneId> = panes.iter().map(|p| p.pane.pane_id()).collect();
        found.sort();
        assert_eq!(vec![1, 2, 3, 4], found);
        for p in &panes {
            assert!(p.width > 0 && p.height > 0, "pane {} collapsed", p.index);
        }
    }

    #[test]
    fn dock_unit_rejects_a_target_that_is_not_a_whole_subtree() {
        let (tab, _) = three_across();
        assert!(!tab.dock_unit(&[3], &[1, 2], dock_right()));
        assert_eq!(ids(&tab), vec![1, 2, 3], "nothing was taken out");
    }

    #[test]
    fn dock_unit_rejects_overlapping_source_and_target() {
        let (tab, _) = three_across();
        assert!(!tab.dock_unit(&[2, 3], &[3], dock_right()));
        assert!(!tab.dock_unit(&[], &[3], dock_right()));
        assert!(!tab.dock_unit(&[99], &[1], dock_right()));
        assert_eq!(ids(&tab), vec![1, 2, 3]);
    }

    fn activate(tab: &Tab, id: PaneId) {
        let idx = tab
            .iter_panes_ignoring_zoom()
            .iter()
            .find(|p| p.pane.pane_id() == id)
            .unwrap()
            .index;
        tab.set_active_idx(idx);
    }

    fn active_id(tab: &Tab) -> PaneId {
        tab.get_active_pane().unwrap().pane_id()
    }

    fn assert_valid(tab: &Tab) {
        let inner = tab.inner.lock();
        let tree = inner.pane.as_ref().unwrap();
        assert_eq!(
            Ok(()),
            inner.groups.validate(&subtree_leaf_sets(tree), &inner.hidden)
        );
    }

    fn visible_ids(tab: &Tab) -> Vec<PaneId> {
        let mut ids = ids(tab);
        ids.sort();
        ids
    }

    fn group_members(tab: &Tab) -> Vec<Vec<PaneId>> {
        let inner = tab.inner.lock();
        inner
            .groups
            .groups
            .iter()
            .map(|g| {
                let mut m = g.members.clone();
                m.sort();
                m
            })
            .collect()
    }

    /// Two groups, {2,3} and {1,4}, stacked in one slot with {1,4} on show.
    fn stacked_groups() -> Tab {
        let tab = four_panes();
        activate(&tab, 2);
        assert!(tab.group_panes(PaneDirection::Right));
        activate(&tab, 1);
        assert!(tab.group_panes(PaneDirection::Down));
        activate(&tab, 1);
        assert!(tab.stack_join(PaneDirection::Right));
        tab
    }

    #[test]
    fn group_panes_merges_active_pane_with_right_neighbour() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.group_panes(PaneDirection::Right));
        assert_eq!(vec![vec![1, 2]], group_members(&tab));
        assert!(leaf_sets(&tab).contains(&set(&[1, 2])));
        assert_eq!(1, active_id(&tab));
        assert_valid(&tab);
    }

    #[test]
    fn group_panes_into_existing_group_extends_it() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.group_panes(PaneDirection::Right));
        activate(&tab, 3);
        assert!(tab.group_panes(PaneDirection::Left));
        assert_eq!(vec![vec![1, 2, 3]], group_members(&tab));
        assert_valid(&tab);
    }

    #[test]
    fn group_panes_returns_false_with_no_neighbour() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(!tab.group_panes(PaneDirection::Left));
        assert!(!tab.group_panes(PaneDirection::Next));
        assert!(group_members(&tab).is_empty());
    }

    #[test]
    fn group_with_binds_active_pane_to_a_pane_that_is_not_adjacent() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.group_with(3));
        assert_eq!(vec![vec![1, 3]], group_members(&tab));
        assert_eq!(1, active_id(&tab));
        assert_valid(&tab);
    }

    #[test]
    fn group_with_refuses_the_active_pane_and_unknown_panes() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(!tab.group_with(1));
        assert!(!tab.group_with(99));
        assert!(group_members(&tab).is_empty());
    }

    #[test]
    fn stack_join_with_stacks_active_unit_onto_a_far_pane() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.stack_join_with(3));
        assert_eq!(set(&[3]), tab.hidden_pane_ids());
        assert_eq!(1, active_id(&tab));
        assert_valid(&tab);
    }

    #[test]
    fn stack_join_with_refuses_the_active_pane() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(!tab.stack_join_with(1));
        assert!(tab.hidden_pane_ids().is_empty());
    }

    #[test]
    fn pane_unit_info_for_plain_panes_has_no_group_or_stack() {
        let (tab, _) = three_across();
        let info = tab.pane_unit_info();
        for id in 1..=3 {
            assert_eq!(None, info[&id].group);
            assert_eq!(None, info[&id].stack);
            assert!(info[&id].on_show);
        }
    }

    #[test]
    fn pane_unit_info_marks_group_members_with_one_group() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.group_panes(PaneDirection::Right));
        let info = tab.pane_unit_info();
        assert!(info[&1].group.is_some());
        assert_eq!(info[&1].group, info[&2].group);
        assert_eq!(None, info[&3].group);
    }

    #[test]
    fn pane_unit_info_marks_stacked_units_and_which_is_on_show() {
        let tab = stacked_groups();
        let info = tab.pane_unit_info();
        let stack = info[&1].stack;
        assert!(stack.is_some());
        for id in 1..=4 {
            assert_eq!(stack, info[&id].stack);
            assert_eq!(2, info[&id].stack_len);
        }
        assert!(info[&1].on_show && info[&4].on_show);
        assert!(!info[&2].on_show && !info[&3].on_show);
    }

    #[test]
    fn ungroup_active_restores_plain_panes() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.group_panes(PaneDirection::Right));
        assert!(tab.ungroup_active());
        assert!(group_members(&tab).is_empty());
        assert!(!tab.ungroup_active(), "a lone pane has no group to dissolve");
        assert_eq!(vec![1, 2, 3], visible_ids(&tab));
    }

    #[test]
    fn stack_join_hides_previous_unit_and_shows_joined_one() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.stack_join(PaneDirection::Right));
        assert_eq!(set(&[2]), tab.hidden_pane_ids());
        assert_eq!(vec![1, 3], visible_ids(&tab));
        assert_eq!(1, active_id(&tab));
        assert_valid(&tab);
        for p in tab.iter_panes() {
            assert!(p.width > 0 && p.height > 0, "pane {} collapsed", p.index);
        }
    }

    #[test]
    fn stack_join_hidden_panes_leave_exactly_one_visible_unit() {
        let tab = stacked_groups();
        assert_eq!(set(&[2, 3]), tab.hidden_pane_ids());
        assert_eq!(vec![1, 4], visible_ids(&tab));
        assert_valid(&tab);
    }

    #[test]
    fn stack_join_in_a_two_pane_tab_keeps_one_pane_visible() {
        let size = TerminalSize {
            rows: 24,
            cols: 80,
            pixel_width: 800,
            pixel_height: 600,
            dpi: 96,
        };
        let tab = Tab::new(&size);
        tab.assign_pane(&FakePane::new(1, size));
        let split = tab.compute_split_size(0, dock_right()).unwrap();
        tab.split_and_insert(0, dock_right(), FakePane::new(2, split.second))
            .unwrap();
        activate(&tab, 1);
        assert!(tab.stack_join(PaneDirection::Right));
        assert_eq!(vec![1], visible_ids(&tab));
        assert_valid(&tab);
    }

    #[test]
    fn stack_cycle_next_swaps_visible_unit_and_wraps() {
        let tab = stacked_groups();
        assert!(tab.stack_cycle(PaneDirection::Next));
        assert_eq!(vec![2, 3], visible_ids(&tab));
        assert!([2, 3].contains(&active_id(&tab)));
        assert_valid(&tab);
        assert!(tab.stack_cycle(PaneDirection::Next));
        assert_eq!(vec![1, 4], visible_ids(&tab));
        assert!(tab.stack_cycle(PaneDirection::Prev));
        assert_eq!(vec![2, 3], visible_ids(&tab));
        assert_valid(&tab);
    }

    #[test]
    fn stack_cycle_on_single_unit_is_noop_false() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(!tab.stack_cycle(PaneDirection::Next));
        assert!(!tab.stack_cycle(PaneDirection::Right));
        assert_eq!(vec![1, 2, 3], visible_ids(&tab));
    }

    fn stack_count(tab: &Tab) -> usize {
        tab.inner.lock().groups.stacks.len()
    }

    #[test]
    fn lifecycle_removing_one_of_two_group_members_dissolves_group() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.group_panes(PaneDirection::Right));
        tab.remove_pane(2);
        assert!(group_members(&tab).is_empty());
        assert_eq!(vec![1, 3], visible_ids(&tab));
        assert_valid(&tab);
    }

    #[test]
    fn lifecycle_closing_active_unit_in_stack_reveals_next_unit() {
        let tab = stacked_groups();
        tab.remove_pane(1);
        tab.remove_pane(4);
        assert_eq!(0, stack_count(&tab));
        assert!(tab.hidden_pane_ids().is_empty());
        assert_eq!(vec![2, 3], visible_ids(&tab));
        assert!([2, 3].contains(&active_id(&tab)));
        assert_valid(&tab);
        for p in tab.iter_panes() {
            assert!(p.width > 0 && p.height > 0, "pane {} collapsed", p.index);
        }
    }

    #[test]
    fn lifecycle_closing_last_pane_of_hidden_unit_keeps_active_unit_focus() {
        let tab = stacked_groups();
        activate(&tab, 1);
        tab.remove_pane(2);
        tab.remove_pane(3);
        assert_eq!(0, stack_count(&tab));
        assert!(tab.hidden_pane_ids().is_empty(), "dead panes leave the hidden set");
        assert_eq!(vec![1, 4], visible_ids(&tab));
        assert_eq!(1, active_id(&tab));
        assert_eq!(vec![vec![1, 4]], group_members(&tab));
        assert_valid(&tab);
    }

    #[test]
    fn lifecycle_killing_group_member_updates_last_active() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.group_panes(PaneDirection::Right));
        activate(&tab, 3);
        assert!(tab.group_panes(PaneDirection::Left));
        activate(&tab, 2);
        tab.remove_pane(2);
        let inner = tab.inner.lock();
        assert_eq!(vec![1, 3], inner.groups.groups[0].members.iter().copied().fold(vec![], |mut v, m| { v.push(m); v.sort(); v }));
        assert_ne!(2, inner.groups.groups[0].last_active);
    }

    #[test]
    fn lifecycle_set_active_pane_records_last_active_for_its_group() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.group_panes(PaneDirection::Right));
        activate(&tab, 2);
        assert_eq!(2, tab.inner.lock().groups.groups[0].last_active);
    }

    #[test]
    fn lifecycle_activate_pane_direction_never_lands_in_hidden_unit() {
        let tab = stacked_groups();
        activate(&tab, 1);
        for direction in [
            PaneDirection::Left,
            PaneDirection::Right,
            PaneDirection::Up,
            PaneDirection::Down,
            PaneDirection::Next,
            PaneDirection::Prev,
        ] {
            tab.activate_pane_direction(direction);
            assert!([1, 4].contains(&active_id(&tab)), "{:?} reached a hidden pane", direction);
        }
    }

    #[test]
    fn lifecycle_rotate_with_a_group_changes_nothing() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.group_panes(PaneDirection::Right));
        let before = ids(&tab);
        tab.rotate_clockwise();
        assert_eq!(before, ids(&tab));
        assert_valid(&tab);
    }

    #[test]
    fn lifecycle_swap_and_focus_layout_refuse_when_they_would_break_a_group() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.group_panes(PaneDirection::Right));
        assert!(!tab.swap_panes(1, 3));
        assert!(!tab.focus_layout(3, 0.6));
        assert_valid(&tab);

        let stacked = stacked_groups();
        assert!(!stacked.swap_panes(1, 4));
        assert!(!stacked.focus_layout(1, 0.6));
        assert_valid(&stacked);
    }

    #[test]
    fn lifecycle_split_of_a_group_member_joins_the_group() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.group_panes(PaneDirection::Right));
        let req = SplitRequest {
            direction: SplitDirection::Vertical,
            ..Default::default()
        };
        let idx = tab
            .iter_panes()
            .iter()
            .find(|p| p.pane.pane_id() == 1)
            .unwrap()
            .index;
        let split = tab.compute_split_size(idx, req).unwrap();
        tab.split_and_insert(idx, req, FakePane::new(9, split.second))
            .unwrap();
        assert_eq!(vec![vec![1, 2, 9]], group_members(&tab));
        assert_valid(&tab);
    }

    #[test]
    fn group_round_trips_through_session_layout() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.group_panes(PaneDirection::Right));
        let layout = tab.session_layout_tree();

        // The GUI rebuilds the same tree shape, then asks for the groups.
        let (fresh, _) = three_across();
        activate(&fresh, 1);
        assert!(fresh.group_panes(PaneDirection::Right));
        fresh.inner.lock().groups = GroupState::default();
        assert!(fresh.restore_groups(&layout));
        assert_eq!(vec![vec![1, 2]], group_members(&fresh));
        assert_valid(&fresh);
    }

    #[test]
    fn stack_with_hidden_unit_round_trips() {
        let tab = stacked_groups();
        let layout = tab.session_layout_tree();

        let fresh = stacked_groups();
        fresh.inner.lock().groups = GroupState::default();
        assert!(fresh.restore_groups(&layout));
        assert_eq!(1, stack_count(&fresh));
        assert_eq!(vec![1, 4], visible_ids(&fresh));
        assert_valid(&fresh);
        assert!(fresh.stack_cycle(PaneDirection::Next));
        assert_eq!(vec![2, 3], visible_ids(&fresh));
    }

    #[test]
    fn restoring_a_layout_that_does_not_fit_the_tree_leaves_no_groups() {
        let (tab, _) = three_across();
        activate(&tab, 1);
        assert!(tab.group_panes(PaneDirection::Right));
        let layout = tab.session_layout_tree();

        let four = four_panes();
        assert!(!four.restore_groups(&layout));
        assert!(group_members(&four).is_empty());
    }

    fn index_of(tab: &Tab, id: PaneId) -> usize {
        tab.iter_panes_ignoring_zoom()
            .iter()
            .find(|p| p.pane.pane_id() == id)
            .unwrap()
            .index
    }

    #[test]
    fn fix_split_size_in_a_stacked_tab_uses_the_all_pane_index() {
        let tab = stacked_groups();
        let idx = index_of(&tab, 1);
        let split = tab.compute_split_size(idx, dock_right());
        assert!(split.is_some(), "shown pane 1 at index {} must be splittable", idx);
        tab.split_and_insert(idx, dock_right(), FakePane::new(9, split.unwrap().second))
            .unwrap();
        let mut groups = group_members(&tab);
        groups.sort();
        assert_eq!(vec![vec![1, 4, 9], vec![2, 3]], groups);
        assert_valid(&tab);
    }

    #[test]
    fn fix_directional_navigation_works_with_a_pane_hidden() {
        let (tab, _) = three_across();
        tab.toggle_pane_hidden(3);
        activate(&tab, 1);
        tab.activate_pane_direction(PaneDirection::Right);
        assert_eq!(2, active_id(&tab), "the divider cell must count in visible geometry");
        tab.activate_pane_direction(PaneDirection::Left);
        assert_eq!(1, active_id(&tab));
    }

    #[test]
    fn fix_join_works_after_a_stack_exists() {
        let tab = four_panes();
        activate(&tab, 2);
        assert!(tab.group_panes(PaneDirection::Right));
        activate(&tab, 1);
        assert!(tab.stack_join(PaneDirection::Right));
        assert_eq!(vec![1, 4], visible_ids(&tab));
        // 1 now fills the {2,3} slot, to the right of 4; neighbours must
        // still be found with panes hidden.
        activate(&tab, 4);
        assert!(tab.stack_join(PaneDirection::Right));
        assert_eq!(vec![4], visible_ids(&tab));
        assert_eq!(1, stack_count(&tab));
        assert_valid(&tab);
    }

    #[test]
    fn fix_lone_death_in_shown_group_keeps_focus_visible() {
        let tab = stacked_groups();
        activate(&tab, 1);
        tab.remove_pane(1);
        assert!(!tab.hidden_pane_ids().contains(&active_id(&tab)));
        assert_valid(&tab);
    }

    #[test]
    fn fix_next_and_prev_skip_hidden_panes() {
        let tab = stacked_groups();
        for start in [1, 4] {
            for direction in [PaneDirection::Next, PaneDirection::Prev] {
                activate(&tab, start);
                tab.activate_pane_direction(direction);
                assert!(
                    [1, 4].contains(&active_id(&tab)),
                    "{:?} from {} reached a hidden pane",
                    direction,
                    start
                );
            }
        }
    }

    #[test]
    fn fix_toggle_hidden_on_a_stacked_pane_never_splits_a_unit() {
        let tab = stacked_groups();
        tab.toggle_pane_hidden(2);
        assert_eq!(vec![2, 3], visible_ids(&tab));
        assert_valid(&tab);
        tab.toggle_pane_hidden(2);
        assert_eq!(vec![2, 3], visible_ids(&tab), "hiding a shown stacked pane is refused");
        assert_valid(&tab);
    }

    #[test]
    fn fix_clicking_a_hidden_stacked_pane_brings_its_unit_into_view() {
        let tab = stacked_groups();
        tab.set_active_idx(index_of(&tab, 2));
        assert_eq!(vec![2, 3], visible_ids(&tab));
        assert!([2, 3].contains(&active_id(&tab)));
        assert_valid(&tab);
    }

    #[test]
    fn fix_dock_pane_in_a_stacked_tab_keeps_the_model_valid() {
        let tab = stacked_groups();
        assert!(tab.dock_pane(4, 1, dock_right()));
        assert_valid(&tab);
        assert!(tab.hidden_pane_ids().iter().all(|id| [2, 3].contains(id)));
    }

    #[test]
    fn dock_pane_moves_a_pane_next_to_another_and_activates_it() {
        let (tab, _) = three_across();
        let below = SplitRequest {
            direction: SplitDirection::Vertical,
            target_is_second: true,
            top_level: false,
            size: SplitSize::Percent(50),
        };

        // Pane 1 goes under pane 3: 2 | (3 over 1).
        assert!(tab.dock_pane(1, 3, below));
        assert_eq!(ids(&tab), vec![2, 3, 1]);
        let panes = tab.iter_panes();
        let by_id = |id: PaneId| panes.iter().find(|p| p.pane.pane_id() == id).unwrap();
        let (p2, p3, p1) = (by_id(2), by_id(3), by_id(1));
        assert_eq!(p2.left, 0);
        assert_eq!(p3.left, p1.left, "docked pane shares the target's column");
        assert!(p1.top > p3.top, "and sits below it");
        for p in &panes {
            assert!(p.width > 0 && p.height > 0, "pane {} collapsed", p.index);
        }
        assert!(p1.is_active, "the moved pane takes focus");

        // Docking onto itself, or onto a pane that is not there, changes nothing.
        assert!(!tab.dock_pane(1, 1, below));
        assert!(!tab.dock_pane(1, 99, below));
        assert!(!tab.dock_pane(99, 1, below));
        assert_eq!(ids(&tab), vec![2, 3, 1]);
    }

    #[test]
    fn dock_pane_refuses_a_target_too_small_to_split_without_losing_the_pane() {
        let size = TerminalSize {
            rows: 24,
            cols: 5,
            pixel_width: 50,
            pixel_height: 600,
            dpi: 96,
        };
        let tab = Tab::new(&size);
        tab.assign_pane(&FakePane::new(1, size));
        let req = SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, req).unwrap();
        tab.split_and_insert(0, req, FakePane::new(2, split.second)).unwrap();

        // Each pane is about two columns wide: too narrow to split again.
        let left_of = SplitRequest {
            direction: SplitDirection::Horizontal,
            target_is_second: false,
            ..req
        };
        assert!(!tab.dock_pane(1, 2, left_of));
        assert_eq!(ids(&tab), vec![1, 2], "nothing was taken out");
    }

    #[test]
    fn tab_splitting() {
        let size = TerminalSize {
            rows: 24,
            cols: 80,
            pixel_width: 800,
            pixel_height: 600,
            dpi: 96,
        };

        let tab = Tab::new(&size);
        tab.assign_pane(&FakePane::new(1, size));

        let panes = tab.iter_panes();
        assert_eq!(1, panes.len());
        assert_eq!(0, panes[0].index);
        assert_eq!(true, panes[0].is_active);
        assert_eq!(0, panes[0].left);
        assert_eq!(0, panes[0].top);
        assert_eq!(80, panes[0].width);
        assert_eq!(24, panes[0].height);

        assert!(tab
            .compute_split_size(
                1,
                SplitRequest {
                    direction: SplitDirection::Horizontal,
                    ..Default::default()
                }
            )
            .is_none());

        let horz_size = tab
            .compute_split_size(
                0,
                SplitRequest {
                    direction: SplitDirection::Horizontal,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            horz_size,
            SplitDirectionAndSize {
                direction: SplitDirection::Horizontal,
                second: TerminalSize {
                    rows: 24,
                    cols: 40,
                    pixel_width: 400,
                    pixel_height: 600,
                    dpi: 96,
                },
                first: TerminalSize {
                    rows: 24,
                    cols: 39,
                    pixel_width: 390,
                    pixel_height: 600,
                    dpi: 96,
                },
            }
        );

        let vert_size = tab
            .compute_split_size(
                0,
                SplitRequest {
                    direction: SplitDirection::Vertical,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            vert_size,
            SplitDirectionAndSize {
                direction: SplitDirection::Vertical,
                second: TerminalSize {
                    rows: 12,
                    cols: 80,
                    pixel_width: 800,
                    pixel_height: 300,
                    dpi: 96,
                },
                first: TerminalSize {
                    rows: 11,
                    cols: 80,
                    pixel_width: 800,
                    pixel_height: 275,
                    dpi: 96,
                }
            }
        );

        let new_index = tab
            .split_and_insert(
                0,
                SplitRequest {
                    direction: SplitDirection::Horizontal,
                    ..Default::default()
                },
                FakePane::new(2, horz_size.second),
            )
            .unwrap();
        assert_eq!(new_index, 1);

        let panes = tab.iter_panes();
        assert_eq!(2, panes.len());

        assert_eq!(0, panes[0].index);
        assert_eq!(false, panes[0].is_active);
        assert_eq!(0, panes[0].left);
        assert_eq!(0, panes[0].top);
        assert_eq!(39, panes[0].width);
        assert_eq!(24, panes[0].height);
        assert_eq!(390, panes[0].pixel_width);
        assert_eq!(600, panes[0].pixel_height);
        assert_eq!(1, panes[0].pane.pane_id());

        assert_eq!(1, panes[1].index);
        assert_eq!(true, panes[1].is_active);
        assert_eq!(40, panes[1].left);
        assert_eq!(0, panes[1].top);
        assert_eq!(40, panes[1].width);
        assert_eq!(24, panes[1].height);
        assert_eq!(400, panes[1].pixel_width);
        assert_eq!(600, panes[1].pixel_height);
        assert_eq!(2, panes[1].pane.pane_id());

        let vert_size = tab
            .compute_split_size(
                0,
                SplitRequest {
                    direction: SplitDirection::Vertical,
                    ..Default::default()
                },
            )
            .unwrap();
        let new_index = tab
            .split_and_insert(
                0,
                SplitRequest {
                    direction: SplitDirection::Vertical,
                    top_level: false,
                    target_is_second: true,
                    size: Default::default(),
                },
                FakePane::new(3, vert_size.second),
            )
            .unwrap();
        assert_eq!(new_index, 1);

        let panes = tab.iter_panes();
        assert_eq!(3, panes.len());

        assert_eq!(0, panes[0].index);
        assert_eq!(false, panes[0].is_active);
        assert_eq!(0, panes[0].left);
        assert_eq!(0, panes[0].top);
        assert_eq!(39, panes[0].width);
        assert_eq!(11, panes[0].height);
        assert_eq!(390, panes[0].pixel_width);
        assert_eq!(275, panes[0].pixel_height);
        assert_eq!(1, panes[0].pane.pane_id());

        assert_eq!(1, panes[1].index);
        assert_eq!(true, panes[1].is_active);
        assert_eq!(0, panes[1].left);
        assert_eq!(12, panes[1].top);
        assert_eq!(39, panes[1].width);
        assert_eq!(12, panes[1].height);
        assert_eq!(390, panes[1].pixel_width);
        assert_eq!(300, panes[1].pixel_height);
        assert_eq!(3, panes[1].pane.pane_id());

        assert_eq!(2, panes[2].index);
        assert_eq!(false, panes[2].is_active);
        assert_eq!(40, panes[2].left);
        assert_eq!(0, panes[2].top);
        assert_eq!(40, panes[2].width);
        assert_eq!(24, panes[2].height);
        assert_eq!(400, panes[2].pixel_width);
        assert_eq!(600, panes[2].pixel_height);
        assert_eq!(2, panes[2].pane.pane_id());

        tab.resize_split_by(1, 1);
        let panes = tab.iter_panes();
        assert_eq!(39, panes[0].width);
        assert_eq!(12, panes[0].height);
        assert_eq!(390, panes[0].pixel_width);
        assert_eq!(300, panes[0].pixel_height);

        assert_eq!(39, panes[1].width);
        assert_eq!(11, panes[1].height);
        assert_eq!(390, panes[1].pixel_width);
        assert_eq!(275, panes[1].pixel_height);

        assert_eq!(40, panes[2].width);
        assert_eq!(24, panes[2].height);
        assert_eq!(400, panes[2].pixel_width);
        assert_eq!(600, panes[2].pixel_height);
    }

    fn is_send_and_sync<T: Send + Sync>() -> bool {
        true
    }

    #[test]
    fn tab_is_send_and_sync() {
        assert!(is_send_and_sync::<Tab>());
    }
}
