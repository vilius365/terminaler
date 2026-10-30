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

/// What a pane's removal asks the tab to do.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Removal {
    /// A hidden unit that must now be shown, because the removed pane took
    /// the stack's visible unit with it.
    pub reveal: Option<Unit>,
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

    /// Forget `pane`. A group left with one member dissolves into a lone
    /// pane, and a unit that vanishes leaves its stack. A stack left with
    /// fewer than two units is dropped.
    pub fn remove_pane(&mut self, pane: PaneId) -> Removal {
        let mut vanished = None;
        let mut renamed = None;
        match self.unit_of(pane) {
            Unit::Group(id) => {
                let Some(group) = self.groups.iter_mut().find(|g| g.id == id) else {
                    return Removal::default();
                };
                group.members.retain(|m| *m != pane);
                match group.members.first().copied() {
                    None => vanished = Some(Unit::Group(id)),
                    Some(first) => {
                        if group.last_active == pane {
                            group.last_active = first;
                        }
                        if group.members.len() == 1 {
                            renamed = Some((Unit::Group(id), Unit::Pane(first)));
                        }
                    }
                }
                if vanished.is_some() || renamed.is_some() {
                    self.dissolve(id);
                }
            }
            unit @ Unit::Pane(_) => vanished = Some(unit),
        }

        if let Some((from, to)) = renamed {
            for unit in self.stacks.iter_mut().flat_map(|s| s.units.iter_mut()) {
                if *unit == from {
                    *unit = to.clone();
                }
            }
        }

        let mut reveal = None;
        if let Some(gone) = vanished {
            if let Some(index) = self.stack_of(&gone) {
                let stack = &mut self.stacks[index];
                if let Some(pos) = stack.units.iter().position(|u| *u == gone) {
                    stack.units.remove(pos);
                    if pos == stack.active && !stack.units.is_empty() {
                        stack.active = pos.min(stack.units.len() - 1);
                        reveal = Some(stack.units[stack.active].clone());
                    } else if pos < stack.active {
                        stack.active -= 1;
                    }
                }
                if stack.units.len() < 2 {
                    self.stacks.remove(index);
                }
            }
        }
        Removal { reveal }
    }

    /// `new` was split off `existing` and now sits inside its subtree, so it
    /// joins `existing`'s unit. A lone pane that is not stacked stays lone.
    pub fn adopt(&mut self, existing: PaneId, new: PaneId) {
        match self.unit_of(existing) {
            Unit::Group(id) => {
                if let Some(group) = self.groups.iter_mut().find(|g| g.id == id) {
                    group.members.push(new);
                }
            }
            lone @ Unit::Pane(_) => {
                if self.stack_of(&lone).is_none() {
                    return;
                }
                let id = self.create_group(vec![existing, new]);
                if let Some(group) = self.groups.iter_mut().find(|g| g.id == id) {
                    group.last_active = new;
                }
                for unit in self.stacks.iter_mut().flat_map(|s| s.units.iter_mut()) {
                    if *unit == lone {
                        *unit = Unit::Group(id);
                    }
                }
            }
        }
    }

    /// `pane` took focus: its group, if any, remembers it.
    pub fn note_active(&mut self, pane: PaneId) {
        if let Some(group) = self.groups.iter_mut().find(|g| g.members.contains(&pane)) {
            group.last_active = pane;
        }
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
    fn stack_of_units(units: Vec<Unit>, active: usize) -> GroupState {
        let mut state = GroupState::default();
        state.stacks.push(PaneStack { units, active });
        state
    }

    #[test]
    fn removing_from_a_three_member_group_keeps_the_group() {
        let mut state = GroupState::default();
        let id = state.create_group(vec![1, 2, 3]);
        state.groups[0].last_active = 2;
        assert_eq!(Removal::default(), state.remove_pane(2));
        assert_eq!(vec![1, 3], state.groups[0].members);
        assert_ne!(2, state.groups[0].last_active);
        assert_eq!(Unit::Group(id), state.unit_of(1));
    }

    #[test]
    fn removing_from_a_two_member_group_dissolves_it_and_renames_its_stack_unit() {
        let mut state = stack_of_units(vec![Unit::Pane(5), Unit::Pane(1)], 1);
        let id = state.create_group(vec![1, 2]);
        state.stacks[0].units[1] = Unit::Group(id);
        state.remove_pane(2);
        assert!(state.groups.is_empty());
        assert_eq!(vec![Unit::Pane(5), Unit::Pane(1)], state.stacks[0].units);
    }

    #[test]
    fn removing_the_active_unit_of_a_two_unit_stack_reveals_the_other_and_drops_the_stack() {
        let mut state = stack_of_units(vec![Unit::Pane(1), Unit::Pane(2)], 1);
        let removal = state.remove_pane(2);
        assert_eq!(Some(Unit::Pane(1)), removal.reveal);
        assert!(state.stacks.is_empty());
    }

    #[test]
    fn removing_a_hidden_unit_drops_the_stack_without_revealing() {
        let mut state = stack_of_units(vec![Unit::Pane(1), Unit::Pane(2)], 1);
        let removal = state.remove_pane(1);
        assert_eq!(None, removal.reveal);
        assert!(state.stacks.is_empty());
    }

    #[test]
    fn removing_the_active_unit_of_a_three_unit_stack_reveals_the_next() {
        let mut state = stack_of_units(vec![Unit::Pane(1), Unit::Pane(2), Unit::Pane(3)], 1);
        let removal = state.remove_pane(2);
        assert_eq!(Some(Unit::Pane(3)), removal.reveal);
        assert_eq!(1, state.stacks[0].active);
        assert_eq!(vec![Unit::Pane(1), Unit::Pane(3)], state.stacks[0].units);
    }

    #[test]
    fn adopt_joins_the_existing_panes_group() {
        let mut state = GroupState::default();
        state.create_group(vec![1, 2]);
        state.adopt(2, 9);
        assert_eq!(vec![1, 2, 9], state.groups[0].members);
    }

    #[test]
    fn adopt_wraps_a_stacked_lone_pane_in_a_group() {
        let mut state = stack_of_units(vec![Unit::Pane(1), Unit::Pane(2)], 0);
        state.adopt(1, 9);
        assert_eq!(1, state.groups.len());
        let id = state.groups[0].id;
        assert_eq!(vec![Unit::Group(id), Unit::Pane(2)], state.stacks[0].units);
        assert_eq!(vec![1, 9], state.groups[0].members);
    }

    #[test]
    fn adopt_ignores_a_lone_unstacked_pane() {
        let mut state = GroupState::default();
        state.adopt(1, 9);
        assert!(state.groups.is_empty());
    }

    #[test]
    fn note_active_records_last_active_for_members_only() {
        let mut state = GroupState::default();
        state.create_group(vec![1, 2]);
        state.note_active(2);
        assert_eq!(2, state.groups[0].last_active);
        state.note_active(7);
        assert_eq!(2, state.groups[0].last_active);
    }
}
