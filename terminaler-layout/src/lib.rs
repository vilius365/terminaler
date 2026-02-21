//! Snap layout system for Terminaler.
//!
//! Provides declarative layout descriptions that can be materialized
//! into pane split operations on a Tab.

use serde::{Deserialize, Serialize};

/// Direction of a split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

/// A layout node — either a terminal pane (leaf) or a split (branch).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LayoutNode {
    /// A terminal pane.
    Pane {
        /// Optional command to run in this pane.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        /// Optional working directory.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    /// A split containing two children.
    Split {
        direction: SplitDirection,
        /// Size ratio of first child (0.0–1.0, default 0.5).
        #[serde(default = "default_ratio")]
        ratio: f64,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

fn default_ratio() -> f64 {
    0.5
}

/// A named layout preset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutPreset {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub root: LayoutNode,
}

/// Specification for a pane to be spawned during layout materialization.
#[derive(Debug, Clone)]
pub struct PaneSpec {
    pub command: Option<String>,
    pub cwd: Option<String>,
}

/// A split operation to be applied in sequence to build a layout.
#[derive(Debug, Clone)]
pub struct SplitOp {
    /// Index of the pane to split.
    pub pane_index: usize,
    /// Direction of the split.
    pub direction: SplitDirection,
    /// Ratio for the first child (0.0–1.0).
    pub ratio: f64,
}

/// Returns the 8 built-in layout presets.
pub fn builtin_layouts() -> Vec<LayoutPreset> {
    vec![
        LayoutPreset {
            name: "single".into(),
            description: "Single pane".into(),
            root: LayoutNode::Pane { command: None, cwd: None },
        },
        LayoutPreset {
            name: "hsplit".into(),
            description: "Two panes side by side".into(),
            root: LayoutNode::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.5,
                first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                second: Box::new(LayoutNode::Pane { command: None, cwd: None }),
            },
        },
        LayoutPreset {
            name: "vsplit".into(),
            description: "Two panes stacked".into(),
            root: LayoutNode::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.5,
                first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                second: Box::new(LayoutNode::Pane { command: None, cwd: None }),
            },
        },
        LayoutPreset {
            name: "quad".into(),
            description: "Four equal panes".into(),
            root: LayoutNode::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.5,
                first: Box::new(LayoutNode::Split {
                    direction: SplitDirection::Vertical,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                    second: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                }),
                second: Box::new(LayoutNode::Split {
                    direction: SplitDirection::Vertical,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                    second: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                }),
            },
        },
        LayoutPreset {
            name: "triple-right".into(),
            description: "Main pane left, two stacked right".into(),
            root: LayoutNode::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.6,
                first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                second: Box::new(LayoutNode::Split {
                    direction: SplitDirection::Vertical,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                    second: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                }),
            },
        },
        LayoutPreset {
            name: "triple-bottom".into(),
            description: "Main pane top, two side by side bottom".into(),
            root: LayoutNode::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.6,
                first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                second: Box::new(LayoutNode::Split {
                    direction: SplitDirection::Horizontal,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                    second: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                }),
            },
        },
        LayoutPreset {
            name: "dev".into(),
            description: "Editor main, terminal right, output bottom-right".into(),
            root: LayoutNode::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.55,
                first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                second: Box::new(LayoutNode::Split {
                    direction: SplitDirection::Vertical,
                    ratio: 0.6,
                    first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                    second: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                }),
            },
        },
        LayoutPreset {
            name: "claude-code".into(),
            description: "Main terminal with command pane below".into(),
            root: LayoutNode::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.75,
                first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                second: Box::new(LayoutNode::Pane { command: None, cwd: None }),
            },
        },
    ]
}

/// Count the number of panes in a layout.
pub fn pane_count(node: &LayoutNode) -> usize {
    match node {
        LayoutNode::Pane { .. } => 1,
        LayoutNode::Split { first, second, .. } => pane_count(first) + pane_count(second),
    }
}

/// Collect pane specifications from a layout in order (first child before second).
pub fn collect_panes(node: &LayoutNode) -> Vec<PaneSpec> {
    let mut panes = Vec::new();
    collect_panes_inner(node, &mut panes);
    panes
}

fn collect_panes_inner(node: &LayoutNode, panes: &mut Vec<PaneSpec>) {
    match node {
        LayoutNode::Pane { command, cwd } => {
            panes.push(PaneSpec {
                command: command.clone(),
                cwd: cwd.clone(),
            });
        }
        LayoutNode::Split { first, second, .. } => {
            collect_panes_inner(first, panes);
            collect_panes_inner(second, panes);
        }
    }
}

/// Collect split operations needed to materialize a layout from a single pane.
///
/// The operations should be applied in order. Each operation splits an existing
/// pane (by index) in the given direction with the given ratio. The new pane
/// appears as the second child of the split.
pub fn collect_splits(node: &LayoutNode) -> Vec<SplitOp> {
    let mut ops = Vec::new();
    let mut leaf_count = 0;
    collect_splits_inner(node, &mut ops, &mut leaf_count);
    ops
}

fn collect_splits_inner(node: &LayoutNode, ops: &mut Vec<SplitOp>, leaf_count: &mut usize) {
    match node {
        LayoutNode::Pane { .. } => {
            *leaf_count += 1;
        }
        LayoutNode::Split { direction, ratio, first, second } => {
            let split_target = *leaf_count;
            // Process the first child — it occupies the current pane slot.
            collect_splits_inner(first, ops, leaf_count);
            // Record the split: split the pane at split_target.
            ops.push(SplitOp {
                pane_index: split_target,
                direction: *direction,
                ratio: *ratio,
            });
            // Process the second child.
            collect_splits_inner(second, ops, leaf_count);
        }
    }
}

/// Find a built-in layout by name.
pub fn find_builtin(name: &str) -> Option<LayoutPreset> {
    builtin_layouts().into_iter().find(|l| l.name == name)
}

/// A workspace template combining a layout with pane-specific commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceTemplate {
    /// Name of the workspace template.
    pub name: String,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// Layout to use (references a LayoutPreset by name, or inline).
    pub layout: LayoutNode,
    /// Commands for each pane (matched by pane order in layout).
    /// If fewer commands than panes, remaining panes use default shell.
    #[serde(default)]
    pub pane_commands: Vec<PaneCommand>,
    /// Default working directory for all panes (can be overridden per-pane).
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Command configuration for a single pane in a workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneCommand {
    /// Shell command to run.
    #[serde(default)]
    pub command: Option<String>,
    /// Working directory override for this pane.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Label for this pane.
    #[serde(default)]
    pub label: Option<String>,
}

/// Returns built-in workspace templates.
pub fn builtin_workspaces() -> Vec<WorkspaceTemplate> {
    vec![
        WorkspaceTemplate {
            name: "claude-code".into(),
            description: "Claude Code workspace: main terminal + command pane".into(),
            layout: LayoutNode::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.75,
                first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                second: Box::new(LayoutNode::Pane { command: None, cwd: None }),
            },
            pane_commands: vec![
                PaneCommand {
                    command: None,
                    cwd: None,
                    label: Some("Main Terminal".into()),
                },
                PaneCommand {
                    command: None,
                    cwd: None,
                    label: Some("Command".into()),
                },
            ],
            cwd: None,
        },
        WorkspaceTemplate {
            name: "dev".into(),
            description: "Development workspace: editor + build + terminal".into(),
            layout: LayoutNode::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.55,
                first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                second: Box::new(LayoutNode::Split {
                    direction: SplitDirection::Vertical,
                    ratio: 0.6,
                    first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                    second: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                }),
            },
            pane_commands: vec![
                PaneCommand {
                    command: None,
                    cwd: None,
                    label: Some("Editor".into()),
                },
                PaneCommand {
                    command: None,
                    cwd: None,
                    label: Some("Build".into()),
                },
                PaneCommand {
                    command: None,
                    cwd: None,
                    label: Some("Terminal".into()),
                },
            ],
            cwd: None,
        },
        WorkspaceTemplate {
            name: "monitoring".into(),
            description: "System monitoring: htop + logs + shell".into(),
            layout: LayoutNode::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.5,
                first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                second: Box::new(LayoutNode::Split {
                    direction: SplitDirection::Horizontal,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                    second: Box::new(LayoutNode::Pane { command: None, cwd: None }),
                }),
            },
            pane_commands: vec![
                PaneCommand {
                    command: Some("htop".into()),
                    cwd: None,
                    label: Some("System Monitor".into()),
                },
                PaneCommand {
                    command: None,
                    cwd: None,
                    label: Some("Logs".into()),
                },
                PaneCommand {
                    command: None,
                    cwd: None,
                    label: Some("Shell".into()),
                },
            ],
            cwd: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_count() {
        assert_eq!(builtin_layouts().len(), 8);
    }

    #[test]
    fn test_pane_counts() {
        let layouts = builtin_layouts();
        let expected = [1, 2, 2, 4, 3, 3, 3, 2];
        for (layout, &count) in layouts.iter().zip(expected.iter()) {
            assert_eq!(
                pane_count(&layout.root),
                count,
                "Layout '{}' should have {} panes",
                layout.name,
                count
            );
        }
    }

    #[test]
    fn test_collect_panes_quad() {
        let quad = find_builtin("quad").unwrap();
        let panes = collect_panes(&quad.root);
        assert_eq!(panes.len(), 4);
    }

    #[test]
    fn test_splits_single() {
        let single = find_builtin("single").unwrap();
        assert!(collect_splits(&single.root).is_empty());
    }

    #[test]
    fn test_splits_hsplit() {
        let hsplit = find_builtin("hsplit").unwrap();
        let ops = collect_splits(&hsplit.root);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].pane_index, 0);
        assert_eq!(ops[0].direction, SplitDirection::Horizontal);
    }

    #[test]
    fn test_splits_quad() {
        let quad = find_builtin("quad").unwrap();
        let ops = collect_splits(&quad.root);
        // quad = H(V(p,p), V(p,p)) → 3 splits
        assert_eq!(ops.len(), 3);
    }

    #[test]
    fn test_splits_triple_right() {
        let tr = find_builtin("triple-right").unwrap();
        let ops = collect_splits(&tr.root);
        // H(p, V(p,p)) → 2 splits
        assert_eq!(ops.len(), 2);
    }

    #[test]
    fn test_roundtrip_json() {
        for layout in builtin_layouts() {
            let json = serde_json::to_string(&layout).unwrap();
            let parsed: LayoutPreset = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.name, layout.name);
            assert_eq!(pane_count(&parsed.root), pane_count(&layout.root));
        }
    }

    #[test]
    fn test_find_builtin() {
        assert!(find_builtin("quad").is_some());
        assert!(find_builtin("nonexistent").is_none());
    }

    #[test]
    fn test_builtin_workspaces_count() {
        assert_eq!(builtin_workspaces().len(), 3);
    }

    #[test]
    fn test_workspace_roundtrip() {
        for ws in builtin_workspaces() {
            let json = serde_json::to_string(&ws).unwrap();
            let parsed: WorkspaceTemplate = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.name, ws.name);
        }
    }
}
