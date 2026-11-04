//! Separate SSH read-only modal. No callbacks enter the local workspace.
use crate::{Frost, Message};
use iced::widget::{
    button, column, container, mouse_area, opaque, row, scrollable, stack, text, text_input, Space,
};
use iced::{Element, Length};
use jterm_core::agent_workspace::{ConversationId, WorkspaceSearchHit};

#[derive(Default)]
pub(crate) struct RemotePanel {
    pub(crate) open: bool,
    pub(crate) alias: String,
    pub(crate) executable: String,
    pub(crate) host_id: String,
    pub(crate) query: String,
    pub(crate) generation: u64,
    pub(crate) target: String,
    pub(crate) reader: crate::workspace_remote::RemoteWorkspace,
}
#[derive(Clone, Debug)]
pub(crate) enum RemoteAction {
    Alias(String),
    Executable(String),
    Host(String),
    Connect,
    Disconnect,
    Query(String),
    Scope(ConversationId),
    All,
    Refresh,
    MoreCatalog,
    MoreSearch,
    Hit(Box<WorkspaceSearchHit>),
    CloseEvidence,
}
impl Frost {
    pub(crate) fn workspace_remote_action(&mut self, generation: u64, action: RemoteAction) {
        let panel = &mut self.agent_sessions.remote;
        if !panel.open || panel.generation != generation {
            return;
        }
        match action {
            RemoteAction::Alias(value) if value.len() <= 1024 => panel.alias = value,
            RemoteAction::Executable(value) if value.len() <= 4096 => panel.executable = value,
            RemoteAction::Host(value) if value.len() <= 4096 => panel.host_id = value,
            RemoteAction::Connect => {
                panel.generation = panel.generation.wrapping_add(1);
                panel.target = format!("{} · {}", panel.alias.trim(), panel.host_id.trim());
                panel.query.clear();
                panel
                    .reader
                    .connect(&panel.alias, &panel.executable, &panel.host_id);
            }
            RemoteAction::Disconnect => {
                panel.generation = panel.generation.wrapping_add(1);
                panel.reader.disconnect();
                panel.query.clear();
                panel.target.clear();
            }
            RemoteAction::Query(value) if value.len() <= 8192 => {
                panel.query = value;
                panel
                    .reader
                    .set_search(&panel.query, panel.reader.conversation().cloned());
            }
            RemoteAction::Scope(id) => {
                if panel.reader.rows.iter().any(|row| row.id == id) {
                    panel.reader.set_search(&panel.query, Some(id));
                }
            }
            RemoteAction::All => {
                panel.query.clear();
                panel.reader.set_search("", None);
            }
            RemoteAction::Refresh => panel.reader.refresh(),
            RemoteAction::MoreCatalog => panel.reader.load_more_catalog(),
            RemoteAction::MoreSearch => panel.reader.load_more_search(),
            RemoteAction::Hit(hit) => panel.reader.open_hit(*hit),
            RemoteAction::CloseEvidence => panel.reader.clear_document(),
            _ => {
                panel.reader.notice =
                    Some("That input exceeds the limit. The previous value is retained.".into())
            }
        }
    }
    pub(crate) fn workspace_remote_view(&self) -> Element<'_, Message> {
        let size = self.chrome_font_size();
        let panel = &self.agent_sessions.remote;
        let reader = &panel.reader;
        let generation = panel.generation;
        let action = move |action| Message::WorkspaceRemoteAction(generation, action);
        let mut body = column![
            row![text("Remote workspace · read-only").size(size + 2.0), Space::new().width(Length::Fill), button(text("Close").size(size)).on_press(Message::WorkspaceRemoteClose)].spacing(8).wrap(),
            text("Connect to an already running workspace host. SSH identity and protocol are checked before reading its catalog.").size(size),
            text("SSH config alias").size(size - 1.0),
            text_input("dev-host", &panel.alias).size(size).on_input(move |value| action(RemoteAction::Alias(value))),
            text("Absolute remote frontend executable").size(size - 1.0),
            text_input("/absolute/path/to/frontend", &panel.executable).size(size).on_input(move |value| action(RemoteAction::Executable(value))),
            text("Expected host ID").size(size - 1.0),
            text_input("Host identity", &panel.host_id).size(size).on_input(move |value| action(RemoteAction::Host(value))),
            text("Obtain the ID on that host with: frontend --jterm-workspace-host-id. Known-host verification and noninteractive SSH authentication remain enabled.").size(size - 1.0),
            row![button(text("Connect read-only").size(size)).on_press(action(RemoteAction::Connect)), button(text("Disconnect").size(size)).on_press(action(RemoteAction::Disconnect))].spacing(6).wrap(),
        ].spacing(8);
        if let Some(notice) = &reader.notice {
            body = body.push(text(notice).size(size));
        }
        if reader.loading || reader.search_loading || reader.document_loading {
            body = body.push(text("Reading remote host…").size(size));
        }
        if reader.connected {
            body = body.push(text(format!("Verified remote · {}", panel.target)).size(size + 1.0))
                .push(text("Viewing does not resume or launch work. Context saves, queue writes, SSH task execution and DLC execution are unavailable here.").size(size - 1.0))
                .push(text_input("Search remote saved evidence", &panel.query).size(size).on_input(move |value| action(RemoteAction::Query(value))))
                .push(row![button(text("All remote sessions").size(size)).on_press(action(RemoteAction::All)),
                    button(text("Refresh catalog").size(size)).on_press_maybe((!reader.loading).then(|| action(RemoteAction::Refresh)))].spacing(6).wrap());
            if reader.searching() {
                for hit in &reader.page.hits {
                    body = body.push(
                        button(
                            column![
                                text(crate::review_text::visible_bounded(&hit.row.title, 100))
                                    .size(size),
                                text(hit.source.label()).size(size - 2.0),
                                text(crate::review_text::visible_bounded(&hit.snippet, 220))
                                    .size(size - 1.0),
                            ]
                            .spacing(3),
                        )
                        .on_press(action(RemoteAction::Hit(Box::new(hit.clone()))))
                        .width(Length::Fill)
                        .style(button::text),
                    );
                }
                body = body.push(text(reader.page.coverage.label()).size(size - 1.0));
                if reader.page.next_before.is_some() {
                    body = body.push(button(text("More evidence").size(size)).on_press_maybe(
                        (!reader.search_loading).then(|| action(RemoteAction::MoreSearch)),
                    ));
                }
            } else {
                body = body.push(
                    text(format!(
                        "{} of {} remote sessions loaded",
                        reader.rows.len(),
                        reader.total
                    ))
                    .size(size - 1.0),
                );
                for item in &reader.rows {
                    body = body.push(
                        button(
                            column![
                                text(crate::review_text::visible_bounded(&item.title, 100))
                                    .size(size),
                                text(format!(
                                    "{} · {}",
                                    item.provider,
                                    item.presentation().status_label
                                ))
                                .size(size - 1.0)
                            ]
                            .spacing(3),
                        )
                        .on_press(action(RemoteAction::Scope(item.id.clone())))
                        .width(Length::Fill)
                        .style(button::text),
                    );
                }
                if reader.next_before.is_some() {
                    body = body.push(button(text("More sessions").size(size)).on_press_maybe(
                        (!reader.loading).then(|| action(RemoteAction::MoreCatalog)),
                    ));
                }
            }
            if let Some(document) = &reader.document {
                body = body
                    .push(text("Remote saved evidence").size(size + 1.0))
                    .push(
                        text(format!(
                            "{} · {} · revision {}",
                            document.source.label(),
                            document.event_id,
                            document.revision
                        ))
                        .size(size - 1.0),
                    )
                    .push(
                        text(crate::review_text::visible_multiline_bounded(
                            &document.text,
                            256 * 1024,
                        ))
                        .size(size),
                    );
                if document.truncated {
                    body = body
                        .push(text("Retained excerpt; the source contains more.").size(size - 1.0));
                }
                body = body.push(
                    button(text("Close remote evidence").size(size))
                        .on_press(action(RemoteAction::CloseEvidence)),
                );
            }
        }
        let panel = container(scrollable(body).height(Length::Fill))
            .padding(12)
            .width((self.win_size.width - 24.0).clamp(220.0, 740.0))
            .height((self.win_size.height - 36.0).clamp(180.0, 760.0))
            .style(self.popup_style());
        let background = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::WorkspaceRemoteClose);
        let centered = container(opaque(panel))
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        crate::widget_identity_scope::scope_key(
            ("remote-workspace", generation),
            stack![background, centered],
        )
    }
}
