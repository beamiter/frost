//! frost's binding to the shared AI chat library state.
//!
//! The state machine itself — per-chat provider history, Block context,
//! drafts, archive state, `(chat_id, epoch)` request tokens, the streamed
//! partial reply, the live byte budgets and the compaction that persistence
//! depends on — lives in [`jterm_core::ai::ChatStore`]. That store is the
//! union of the four jterm terminals' previously duplicated copies (anvil,
//! forge, ember and this one), which contained no toolkit code at all and had
//! drifted apart in exactly the ways an unshared state machine does.
//!
//! What is left here is the one decision frost owns: which
//! [`BusyChatPolicy`] its library is built
//! with. frost's panel has no cancel-then-mutate step — Archive and Delete are
//! single clicks and Stop is a separate button the user must press first — so
//! a chat with a request in flight refuses both, and `ai_chats` turns the
//! refusal into "Stop this response before …". Pinning the policy at every
//! construction site keeps that contract explicit rather than inherited from
//! the core's default.

use jterm_core::ai::{BusyChatPolicy, ConversationSnapshot};

pub(crate) use jterm_core::ai::{ChatStatus, ChatStore, ChatStoreError, RequestToken};

/// See the module docs: frost refuses archive/delete on a busy chat.
const BUSY_POLICY: BusyChatPolicy = BusyChatPolicy::Refuse;

/// A fresh library under frost's busy policy.
pub(crate) fn new_store() -> ChatStore {
    ChatStore::with_busy_policy(BUSY_POLICY)
}

/// A persisted library restored under frost's busy policy.
pub(crate) fn restore_store(snapshot: ConversationSnapshot) -> ChatStore {
    ChatStore::restore_with_busy_policy(snapshot, BUSY_POLICY)
}
