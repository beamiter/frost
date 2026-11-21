//! Session-local leases for relative transcript scrolling across safe repaints.
//! Never use this token to authorize a pixel/cell action or PTY input.
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Identity(Arc<()>);
impl Default for Identity {
    fn default() -> Self {
        Self(Arc::new(()))
    }
}
impl PartialEq for Identity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for Identity {}

/// Rotated by central budget failures and terminal ownership transitions.
/// This includes failures recovered before the session observes an unavailable
/// view. Not a counter:
/// retaining a queued token prevents ABA by address reuse or integer wrap.
#[derive(Debug, Default)]
pub struct ScrollGuard {
    identity: Identity,
}
impl ScrollGuard {
    pub fn snapshot(&self) -> Identity {
        self.identity.clone()
    }
    pub fn revoke(&mut self) {
        self.identity = Identity::default();
    }
}

/// Scope intentionally excludes content grid/history revisions and scroll
/// offset. A new row identity remains a conservative revocation, even when a
/// specific append/trim might have been safe to rebind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Domain {
    pub rows: usize,
    pub cols: usize,
    pub projection_mode: u8,
    pub alternate_screen: bool,
    pub row_identity: u64,
    pub policy_revision: u64,
    pub host_policy_revision: u64,
    pub block_spacing: bool,
    pub inline_scope_floor: Option<usize>,
    pub command_generation: Option<u64>,
    pub app_mouse_enabled: bool,
}

#[derive(Clone, Debug)]
pub struct Token {
    session: Identity,
    pane_epoch: u64,
    lease: Identity,
    boundary: Identity,
}

#[derive(Debug, Default)]
pub struct Admission {
    state: Option<(Domain, Identity, Identity)>,
}
impl Admission {
    /// Only pass Some when current projection_interaction_allowed, canvas
    /// enabled and canvas head all hold. None retires the previous lease.
    pub fn observe(&mut self, domain: Option<Domain>, boundary: Identity) {
        match domain {
            None => self.state = None,
            Some(domain) => {
                if self.state.as_ref().is_some_and(|(old, _, old_boundary)| {
                    old == &domain && old_boundary == &boundary
                }) {
                    return;
                }
                self.state = Some((domain, Identity::default(), boundary));
            }
        }
    }
    pub fn capture(&self, session: &Identity, pane_epoch: u64) -> Option<Token> {
        let (_, lease, boundary) = self.state.as_ref()?;
        Some(Token {
            session: session.clone(),
            pane_epoch,
            lease: lease.clone(),
            boundary: boundary.clone(),
        })
    }
    /// Recheck current semantic scope and central failure identity at dispatch,
    /// not merely the last refresh. Caller still checks visible layout, modal
    /// input ownership, current safe projection and finite delta.
    pub fn permits(
        &self,
        token: &Token,
        session: &Identity,
        pane_epoch: u64,
        current_domain: Option<&Domain>,
        current_boundary: &Identity,
    ) -> bool {
        let Some((domain, lease, boundary)) = &self.state else {
            return false;
        };
        token.session == *session
            && token.pane_epoch == pane_epoch
            && token.lease == *lease
            && token.boundary == *boundary
            && token.boundary == *current_boundary
            && current_domain == Some(domain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn domain() -> Domain {
        Domain {
            rows: 25,
            cols: 80,
            projection_mode: 1,
            alternate_screen: false,
            row_identity: 7,
            policy_revision: 8,
            host_policy_revision: 9,
            block_spacing: false,
            inline_scope_floor: Some(10),
            command_generation: Some(11),
            app_mouse_enabled: false,
        }
    }
    fn setup() -> (Admission, ScrollGuard, Identity, Token) {
        let mut admission = Admission::default();
        let guard = ScrollGuard::default();
        let session = Identity::default();
        admission.observe(Some(domain()), guard.snapshot());
        let token = admission.capture(&session, 4).unwrap();
        (admission, guard, session, token)
    }
    #[test]
    fn content_repaints_and_relative_scroll_refresh_keep_lease() {
        let (mut a, g, s, t) = setup();
        for _ in 0..100 {
            a.observe(Some(domain()), g.snapshot());
            assert!(a.permits(&t, &s, 4, Some(&domain()), &g.snapshot()));
        }
    }
    #[test]
    fn budget_failure_and_recovery_revokes_even_when_unavailable_was_not_observed() {
        let (mut a, mut g, s, t) = setup();
        g.revoke();
        assert!(!a.permits(&t, &s, 4, Some(&domain()), &g.snapshot()));
        a.observe(Some(domain()), g.snapshot());
        assert!(!a.permits(&t, &s, 4, Some(&domain()), &g.snapshot()));
        assert!(a.permits(
            &a.capture(&s, 4).unwrap(),
            &s,
            4,
            Some(&domain()),
            &g.snapshot()
        ));
    }
    #[test]
    fn unavailable_or_parked_canvas_cannot_revive_token() {
        let (mut a, g, s, t) = setup();
        a.observe(None, g.snapshot());
        assert!(!a.permits(&t, &s, 4, None, &g.snapshot()));
        a.observe(Some(domain()), g.snapshot());
        assert!(!a.permits(&t, &s, 4, Some(&domain()), &g.snapshot()));
    }
    #[test]
    fn replacement_session_with_reused_numeric_id_is_not_same_owner() {
        let (a, g, _, t) = setup();
        assert!(!a.permits(&t, &Identity::default(), 4, Some(&domain()), &g.snapshot()));
    }
    #[test]
    fn pane_switch_and_back_requires_fresh_epoch() {
        let (a, g, s, t) = setup();
        for epoch in [5, 6] {
            assert!(!a.permits(&t, &s, epoch, Some(&domain()), &g.snapshot()));
        }
    }
    #[test]
    fn size_mode_policy_scope_mouse_or_row_identity_changes_revoke() {
        let mut changes = vec![];
        let mut d = domain();
        d.rows += 1;
        changes.push(d);
        let mut d = domain();
        d.cols += 1;
        changes.push(d);
        let mut d = domain();
        d.projection_mode = 2;
        changes.push(d);
        let mut d = domain();
        d.alternate_screen = true;
        changes.push(d);
        let mut d = domain();
        d.row_identity += 1;
        changes.push(d);
        let mut d = domain();
        d.policy_revision += 1;
        changes.push(d);
        let mut d = domain();
        d.host_policy_revision += 1;
        changes.push(d);
        let mut d = domain();
        d.block_spacing = true;
        changes.push(d);
        let mut d = domain();
        d.inline_scope_floor = Some(11);
        changes.push(d);
        let mut d = domain();
        d.command_generation = Some(12);
        changes.push(d);
        let mut d = domain();
        d.app_mouse_enabled = true;
        changes.push(d);
        for changed in changes {
            let (mut a, g, s, t) = setup();
            assert!(!a.permits(&t, &s, 4, Some(&changed), &g.snapshot()));
            a.observe(Some(changed), g.snapshot());
            a.observe(Some(domain()), g.snapshot());
            assert!(!a.permits(&t, &s, 4, Some(&domain()), &g.snapshot()));
        }
    }
    #[test]
    fn unavailable_dispatch_rejects_even_without_refresh_observation() {
        let (a, g, s, t) = setup();
        assert!(!a.permits(&t, &s, 4, None, &g.snapshot()));
    }

    #[test]
    fn selected_reader_tab_return_rejects_stale_intent_but_accepts_first_full_page() {
        use crate::transcript_canvas::TranscriptCanvas;
        use std::time::{Duration, Instant};

        let scope = domain();
        let (admission, boundary, reader_session, old) = setup();
        let sibling_session = Identity::default();
        let (rows, cols) = (scope.rows, scope.cols);
        // These identities and epochs model the caller; native routing is tested separately.

        let mut reader = TranscriptCanvas::default();
        let mut sibling = TranscriptCanvas::default();
        let start = Instant::now();
        reader.reconcile(40, 100, rows, cols);
        reader.applied(40);
        reader.scroll(0.25, true, start);
        reader.set_text_selection_active(true);
        reader.scroll(20.0, false, start);
        reader.advance(start + Duration::from_millis(30));
        reader.applied(reader.row());
        assert!(reader.animating());
        let parked = reader.position_rows();

        // No advancing frames for the first tab while fresh output and a sibling exist.
        for appended in 1..=16 {
            reader.reconcile(reader.row() + 1, 100 + appended, rows, cols);
            reader.applied(reader.row());
            sibling.reconcile(0, 100 + appended, rows, cols);
            sibling.applied(sibling.row());
        }
        let rebased = reader.position_rows();
        assert!((rebased - parked - 16.0).abs() < 1e-9);
        reader.resume_visible();
        assert_eq!(reader.position_rows(), rebased);
        assert!(!reader.animating());

        let permits = |token, session, epoch| {
            admission.permits(token, session, epoch, Some(&scope), &boundary.snapshot())
        };
        assert!(!permits(&old, &sibling_session, 4));
        assert!(
            !permits(&old, &reader_session, 6),
            "stale pre-switch paging"
        );
        let fresh = admission.capture(&reader_session, 6).unwrap();
        assert!(permits(&fresh, &reader_session, 6));
        let admitted_at = start + Duration::from_secs(1);
        let page = (rows - 2) as f64;
        reader.scroll(page, false, admitted_at);
        assert!(reader.animating());
        assert_eq!(reader.position_rows(), rebased);
        let mut applied_row = reader.row();
        for i in 1..=120 {
            reader.advance(admitted_at + Duration::from_millis(i * 16));
            reader.set_text_selection_active(true);
            reader.reconcile(applied_row + 1, 116 + i as usize, rows, cols);
            applied_row = reader.row();
            reader.applied(applied_row);
        }
        assert!(!reader.animating());
        assert!(!reader.following());
        assert!((reader.position_rows() - rebased - 120.0 - page).abs() < 1e-9);
    }
}
