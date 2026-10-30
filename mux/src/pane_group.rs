//! Pane groups and stacks: a data model laid over a tab's split tree.
//!
//! A unit is a pane or a group. Units that share one layout slot form a
//! stack, and all but the active unit of a stack are hidden.

use crate::pane::PaneId;
use std::collections::HashSet;

pub type GroupId = u32;

/// One layout participant: a lone pane or a whole group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unit {
    Pane(PaneId),
    Group(GroupId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupInfo {
    pub id: GroupId,
    pub members: Vec<PaneId>,
    /// The member that takes focus when the group is focused.
    pub last_active: PaneId,
}

/// Units that share one slot. Exactly one is visible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneStack {
    pub units: Vec<Unit>,
    pub active: usize,
}

#[derive(Clone, Debug, Default)]
pub struct GroupState {
    pub groups: Vec<GroupInfo>,
    pub stacks: Vec<PaneStack>,
    next_id: GroupId,
}

impl GroupState {
    /// The group containing `pane`, or the pane itself as a unit of one.
    pub fn unit_of(&self, pane: PaneId) -> Unit {
        match self.groups.iter().find(|g| g.members.contains(&pane)) {
            Some(g) => Unit::Group(g.id),
            None => Unit::Pane(pane),
        }
    }

    pub fn unit_panes(&self, unit: &Unit) -> Vec<PaneId> {
        match unit {
            Unit::Pane(id) => vec![*id],
            Unit::Group(id) => self
                .groups
                .iter()
                .find(|g| g.id == *id)
                .map(|g| g.members.clone())
                .unwrap_or_default(),
        }
    }

    /// Index into `stacks` of the stack holding `unit`.
    pub fn stack_of(&self, unit: &Unit) -> Option<usize> {
        self.stacks.iter().position(|s| s.units.contains(unit))
    }

    /// Ids are never reused within one state.
    pub fn create_group(&mut self, members: Vec<PaneId>) -> GroupId {
        let id = self.next_id;
        self.next_id += 1;
        let last_active = members.first().copied().unwrap_or_default();
        self.groups.push(GroupInfo {
            id,
            members,
            last_active,
        });
        id
    }

    pub fn dissolve(&mut self, id: GroupId) {
        self.groups.retain(|g| g.id != id);
    }

    /// Checks invariants 1-4 of the spec. `subtrees` holds the leaf set of
    /// every subtree of the tab's split tree.
    pub fn validate(
        &self,
        subtrees: &[HashSet<PaneId>],
        hidden: &HashSet<PaneId>,
    ) -> Result<(), String> {
        let is_subtree = |panes: &HashSet<PaneId>| subtrees.iter().any(|s| s == panes);

        let mut seen = HashSet::new();
        for group in &self.groups {
            if group.members.len() < 2 {
                return Err(format!("group {} has fewer than two members", group.id));
            }
            for pane in &group.members {
                if !seen.insert(*pane) {
                    return Err(format!("pane {} is in more than one group", pane));
                }
            }
            let members: HashSet<PaneId> = group.members.iter().copied().collect();
            if !is_subtree(&members) {
                return Err(format!("group {} is not one subtree", group.id));
            }
        }

        let mut stacked: Vec<&Unit> = vec![];
        for stack in &self.stacks {
            if stack.units.len() < 2 || stack.active >= stack.units.len() {
                return Err("stack has fewer than two units or a bad active index".into());
            }
            let mut union = HashSet::new();
            let mut visible = 0;
            for unit in &stack.units {
                if stacked.contains(&unit) {
                    return Err(format!("unit {:?} is in more than one stack", unit));
                }
                stacked.push(unit);
                let panes = self.unit_panes(unit);
                if panes.is_empty() {
                    return Err(format!("unit {:?} has no panes", unit));
                }
                if panes.iter().all(|p| !hidden.contains(p)) {
                    visible += 1;
                } else if !panes.iter().all(|p| hidden.contains(p)) {
                    return Err(format!("unit {:?} is only partly hidden", unit));
                }
                union.extend(panes);
            }
            if visible != 1 {
                return Err(format!("stack has {} visible units, expected 1", visible));
            }
            if !is_subtree(&union) {
                return Err("stack units do not share one parent subtree".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn set(ids: &[PaneId]) -> HashSet<PaneId> {
        ids.iter().copied().collect()
    }

    /// Leaf sets of every subtree of the tree `((1,2),3)`.
    fn subtrees_12_3() -> Vec<HashSet<PaneId>> {
        vec![set(&[1, 2, 3]), set(&[1, 2]), set(&[1]), set(&[2]), set(&[3])]
    }

    #[test]
    fn group_of_two_adjacent_leaves_validates() {
        let mut state = GroupState::default();
        state.create_group(vec![1, 2]);
        assert_eq!(Ok(()), state.validate(&subtrees_12_3(), &HashSet::new()));
    }

    #[test]
    fn group_with_one_member_fails_validation() {
        let mut state = GroupState::default();
        state.create_group(vec![1]);
        assert!(state.validate(&subtrees_12_3(), &HashSet::new()).is_err());
    }

    #[test]
    fn group_split_across_subtrees_fails_validation() {
        let mut state = GroupState::default();
        state.create_group(vec![1, 3]);
        assert!(state.validate(&subtrees_12_3(), &HashSet::new()).is_err());
    }

    #[test]
    fn pane_in_two_groups_fails_validation() {
        let mut state = GroupState::default();
        state.create_group(vec![1, 2]);
        state.create_group(vec![2, 3]);
        assert!(state.validate(&subtrees_12_3(), &HashSet::new()).is_err());
    }

    #[test]
    fn stack_with_one_visible_unit_validates() {
        let mut state = GroupState::default();
        state.stacks.push(PaneStack {
            units: vec![Unit::Pane(1), Unit::Pane(2)],
            active: 0,
        });
        assert_eq!(Ok(()), state.validate(&subtrees_12_3(), &set(&[2])));
    }

    #[test]
    fn stack_with_two_visible_units_fails_validation() {
        let mut state = GroupState::default();
        state.stacks.push(PaneStack {
            units: vec![Unit::Pane(1), Unit::Pane(2)],
            active: 0,
        });
        assert!(state.validate(&subtrees_12_3(), &HashSet::new()).is_err());
    }

    #[test]
    fn stack_with_no_visible_unit_fails_validation() {
        let mut state = GroupState::default();
        state.stacks.push(PaneStack {
            units: vec![Unit::Pane(1), Unit::Pane(2)],
            active: 0,
        });
        assert!(state.validate(&subtrees_12_3(), &set(&[1, 2])).is_err());
    }

    #[test]
    fn unit_of_returns_group_for_member_and_pane_otherwise() {
        let mut state = GroupState::default();
        let id = state.create_group(vec![1, 2]);
        assert_eq!(Unit::Group(id), state.unit_of(2));
        assert_eq!(Unit::Pane(3), state.unit_of(3));
        assert_eq!(vec![1, 2], state.unit_panes(&Unit::Group(id)));
        assert_eq!(vec![3], state.unit_panes(&Unit::Pane(3)));
    }
}
