use std::sync::{Arc, Weak};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Owner,
    Observer,
}
pub(crate) fn role<I: Copy + Eq>(owner: Option<I>, id: I, observing: bool) -> Option<Role> {
    if owner == Some(id) {
        Some(Role::Owner)
    } else if observing {
        Some(Role::Observer)
    } else {
        None
    }
}

pub(crate) fn title(role: Option<Role>, owner_title: impl FnOnce() -> String) -> String {
    match role {
        Some(Role::Owner) => owner_title(),
        Some(Role::Observer) => "Read-only Frost observer".to_string(),
        None => "Frost".to_string(),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub id: usize,
    generation: Weak<()>,
}
impl Target {
    pub fn capture(id: usize, generation: &Arc<()>) -> Self {
        Self {
            id,
            generation: Arc::downgrade(generation),
        }
    }
    pub fn matches(&self, id: usize, generation: &Arc<()>) -> bool {
        self.id == id
            && Weak::ptr_eq(&self.generation, &Arc::downgrade(generation))
            && self.generation.strong_count() != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unregistered_or_closed_windows_never_fall_back_to_owner() {
        let owner = 1;
        let view = 2;
        assert_eq!(role(Some(owner), owner, false), Some(Role::Owner));
        assert_eq!(role(Some(owner), view, true), Some(Role::Observer));
        assert_eq!(role(Some(owner), view, false), None);
        assert_eq!(role(None, view, false), None);
    }
    #[test]
    fn unregistered_and_observer_factories_cannot_read_owner_title() {
        assert_eq!(title(None, || panic!("borrowed another pane")), "Frost");
        assert_eq!(
            title(Some(Role::Observer), || panic!("borrowed another pane")),
            "Read-only Frost observer"
        );
        assert_eq!(title(Some(Role::Owner), || "owned".to_string()), "owned");
    }
    #[test]
    fn captured_factory_never_borrows_another_or_reused_pane() {
        let original = Arc::new(());
        let other = Arc::new(());
        let target = Target::capture(7, &original);
        assert!(target.matches(7, &original));
        assert!(!target.matches(8, &original));
        assert!(!target.matches(7, &other));
        drop(original);
        assert!(!target.matches(7, &other));
    }
    #[test]
    fn observation_does_not_retain_owner_generation() {
        let owner = Arc::new(());
        let target = Target::capture(1, &owner);
        assert_eq!(Arc::strong_count(&owner), 1);
        drop(owner);
        assert_eq!(target.generation.strong_count(), 0);
    }
}
