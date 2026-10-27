//! Native rich components shared by fixture and observed shell/agent boundaries.
use super::{Message as AppMessage, *};
use crate::reference_icon::{icon, Icon};
use iced::widget::{column, progress_bar};
use jterm_core::reference_conversation::*;
use jterm_core::reference_shell::{self as shell, color};

fn rgb(value: [u8; 3]) -> Color {
    Color::from_rgb8(value[0], value[1], value[2])
}
fn txt(value: impl Into<String>, size: f32, dim: bool, dark: bool) -> iced::widget::Text<'static> {
    text(value.into())
        .font(iced::Font::with_name(shell::FONT_SANS))
        .size(size)
        .color(rgb(match (dim, dark) {
            (true, true) => color::CHROME_MUTED,
            (false, true) => color::CHROME_TEXT,
            (true, false) => color::CANVAS_MUTED,
            (false, false) => color::CANVAS_TEXT,
        }))
}
fn frame(bg: [u8; 3], border: [u8; 3], radius: f32) -> container::Style {
    container::Style {
        background: Some(rgb(bg).into()),
        text_color: Some(rgb(color::CANVAS_TEXT)),
        border: iced::Border {
            color: rgb(border),
            width: 1.0,
            radius: radius.into(),
        },
        ..Default::default()
    }
}
fn mono(value: impl Into<String>, size: f32, dark: bool) -> iced::widget::Text<'static> {
    text(value.into())
        .font(iced::Font::with_name(shell::FONT_MONOSPACE))
        .size(size)
        .color(rgb(if dark {
            color::CHROME_TEXT
        } else {
            color::CANVAS_TEXT
        }))
}

fn elide(value: &str, max: usize) -> String {
    let short: String = value.chars().take(max).collect();
    if value.chars().count() > max {
        format!("{short}…")
    } else {
        short
    }
}

fn part(value: &Part) -> Element<'static, AppMessage> {
    match value {
        Part::Heading(value) => txt(value, shell::type_size::HEADLINE, false, false)
            .font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..iced::Font::with_name(shell::FONT_SANS)
            })
            .into(),
        Part::Paragraph(value) => txt(value, 12.0, false, false).line_height(1.5).into(),
        Part::Metadata(values) => row(values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let progress = index == 5;
                let fg = if progress {
                    [31, 111, 65]
                } else {
                    color::CANVAS_MUTED
                };
                container(
                    row![
                        icon(
                            Icon::from_key(
                                METADATA_ICON_KEYS.get(index).copied().unwrap_or("more")
                            ),
                            rgb(fg),
                            11.0
                        ),
                        txt(value, 9.0, true, false).color(rgb(fg))
                    ]
                    .spacing(4)
                    .align_y(iced::Alignment::Center),
                )
                .padding([2, 6])
                .height(shell::METADATA_PILL_HEIGHT)
                .align_y(iced::Alignment::Center)
                .style(move |_| {
                    frame(
                        if progress {
                            [228, 245, 233]
                        } else {
                            color::ACCENT_PALE
                        },
                        color::LIGHT_BORDER,
                        6.0,
                    )
                })
                .into()
            })
            .collect::<Vec<Element<'static, AppMessage>>>())
        .spacing(shell::METADATA_GAP)
        .wrap()
        .into(),
        Part::Steps(steps) => {
            let rows = steps
                .iter()
                .map(|step| step_row(step, false))
                .collect::<Vec<_>>();
            container(column(rows).spacing(1))
                .style(|_| frame(color::ACCENT_PALE, color::LIGHT_BORDER, 8.0))
                .into()
        }
        Part::Diff(diff) => diff_view(diff),
        Part::Terminal {
            title,
            text: output,
        } => container(column![
            container(
                row![
                    icon(Icon::Terminal, rgb(color::CHROME_MUTED), 13.0),
                    txt(title, 10.0, false, true),
                    Space::new().width(Length::Fill),
                    icon(Icon::ChevronDown, rgb(color::CHROME_MUTED), 12.0)
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center)
            )
            .padding([7, 10]),
            container(mono(output, 10.0, true)).padding([8, 10]),
        ])
        .width(Length::Fill)
        .style(|_| frame(color::TERMINAL, color::DARK_BORDER, 10.0))
        .into(),
    }
}

fn step_row(step: &Step, dark: bool) -> Element<'static, AppMessage> {
    let mark = match step.state {
        StepState::Complete => Icon::Check,
        StepState::Running => Icon::Agent,
        StepState::Queued => Icon::More,
    };
    let state = step.state;
    container(
        row![
            icon(
                mark,
                rgb(if step.state == StepState::Complete {
                    [48, 142, 104]
                } else {
                    color::ACCENT
                }),
                13.0
            ),
            tooltip(
                txt(
                    elide(&step.title, if dark { 35 } else { 80 }),
                    10.0,
                    false,
                    dark
                )
                .width(Length::Fill),
                txt(&step.title, 10.0, false, dark),
                tooltip::Position::Left
            ),
            txt(step.elapsed.as_deref().unwrap_or(""), 9.0, true, dark)
        ]
        .spacing(7)
        .align_y(iced::Alignment::Center),
    )
    .padding([5, 9])
    .height(if dark {
        shell::INSPECTOR_QUEUE_ROW_HEIGHT
    } else {
        shell::TASK_ROW_HEIGHT
    })
    .width(Length::Fill)
    .style(move |_| {
        frame(
            if dark {
                if state == StepState::Running {
                    color::NAVIGATION_SELECTED
                } else {
                    color::INSPECTOR_CARD
                }
            } else {
                color::ACCENT_PALE
            },
            if dark {
                if state == StepState::Running {
                    color::NAVIGATION_SELECTED
                } else {
                    color::INSPECTOR_CARD
                }
            } else {
                color::LIGHT_BORDER
            },
            5.0,
        )
    })
    .into()
}

fn diff_action_style(_: &iced::Theme, status: button::Status) -> button::Style {
    let disabled = matches!(status, button::Status::Disabled);
    button::Style {
        background: Some(
            rgb(
                if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                    color::ACCENT_PALE
                } else {
                    color::CONTROL_SURFACE
                },
            )
            .into(),
        ),
        text_color: rgb(if disabled {
            color::DISABLED_TEXT
        } else {
            color::CANVAS_MUTED
        }),
        border: iced::Border {
            color: rgb(color::CONTROL_BORDER),
            width: 1.0,
            radius: 5.0.into(),
        },
        ..Default::default()
    }
}

fn diff_view(diff: &CodeDiff) -> Element<'static, AppMessage> {
    let copied = diff
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let copy = button(
        row![
            icon(Icon::Copy, rgb(color::CANVAS_MUTED), 12.0),
            txt("Copy", 9.0, true, false)
        ]
        .spacing(4),
    )
    .on_press(AppMessage::ReferenceCopy(copied))
    .padding([3, 6])
    .height(22)
    .style(diff_action_style);
    let apply = tooltip(
        button(txt("Apply", 9.0, true, false))
            .padding([3, 6])
            .height(22)
            .style(diff_action_style),
        txt("No authorized edit target", 10.0, true, false),
        tooltip::Position::Top,
    );
    let header = container(
        row![
            icon(Icon::Code, rgb(color::CANVAS_MUTED), 13.0),
            txt(&diff.path, 10.0, false, false),
            Space::new().width(Length::Fill),
            txt(&diff.language, 9.0, true, false),
            txt(
                format!("+{}  −{}", diff.added, diff.removed),
                9.0,
                true,
                false
            ),
            copy,
            apply
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    )
    .padding([4, 9])
    .height(shell::DIFF_HEADER_HEIGHT)
    .style(|_| frame([242, 246, 253], color::LIGHT_BORDER, 8.0));
    let mut lines = column![];
    for line in &diff.lines {
        let bg = match line.kind {
            DiffKind::Added => [228, 245, 233],
            DiffKind::Removed => [252, 234, 236],
            DiffKind::Context => [251, 252, 255],
        };
        let foreground = match line.kind {
            DiffKind::Added => [31, 111, 65],
            DiffKind::Removed => [164, 54, 66],
            DiffKind::Context => color::CANVAS_TEXT,
        };
        let sign = match line.kind {
            DiffKind::Added => "+",
            DiffKind::Removed => "−",
            DiffKind::Context => " ",
        };
        lines = lines.push(
            container(
                row![
                    mono(
                        line.old_line.map(|n| n.to_string()).unwrap_or_default(),
                        9.0,
                        false
                    )
                    .width(shell::DIFF_GUTTER_COLUMN_WIDTH),
                    mono(
                        line.new_line.map(|n| n.to_string()).unwrap_or_default(),
                        9.0,
                        false
                    )
                    .width(shell::DIFF_GUTTER_COLUMN_WIDTH),
                    mono(sign, 10.0, false)
                        .color(rgb(foreground))
                        .width(shell::DIFF_SIGN_WIDTH),
                    mono(&line.text, 10.0, false).color(rgb(foreground))
                ]
                .spacing(0)
                .align_y(iced::Alignment::Center),
            )
            .padding([0, 8])
            .height(shell::DIFF_LINE_HEIGHT)
            .width(Length::Fill)
            .style(move |_| frame(bg, bg, 0.0)),
        );
    }
    container(column![
        header,
        container(lines).padding(iced::Padding {
            top: shell::DIFF_BODY_VERTICAL_PADDING,
            bottom: shell::DIFF_BODY_VERTICAL_PADDING,
            ..iced::Padding::ZERO
        })
    ])
    .clip(true)
    .style(|_| frame([251, 252, 255], color::LIGHT_BORDER, 10.0))
    .into()
}

pub(crate) fn conversation_view(conversation: &Conversation) -> Element<'static, AppMessage> {
    let mut messages = column![].spacing(12);
    for message in &conversation.messages {
        if message.role == Role::User {
            let paragraphs = message
                .parts
                .iter()
                .filter_map(|part| match part {
                    Part::Paragraph(value) => Some(value.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            messages = messages.push(
                container(
                    row![
                        container(txt(paragraphs, 12.0, false, false))
                            .padding([8, 13])
                            .width(shell::USER_BUBBLE_WIDTH)
                            .height(shell::USER_BUBBLE_HEIGHT)
                            .style(|_| frame(color::USER_BUBBLE, color::USER_BUBBLE, 10.0)),
                        container(txt(&message.avatar, 10.0, false, false).center())
                            .center(24)
                            .width(24)
                            .height(24)
                            .style(|_| frame(color::ACCENT_PALE, color::LIGHT_BORDER, 12.0)),
                    ]
                    .spacing(9)
                    .align_y(iced::Alignment::Center),
                )
                .align_right(Length::Fill)
                .padding(iced::Padding {
                    right: shell::USER_BUBBLE_RIGHT - shell::COMPOSER_INSET - 24.0 - 9.0,
                    ..iced::Padding::ZERO
                }),
            );
        } else {
            let mut parts = column![row![
                txt(&message.author, shell::type_size::ROLE, false, false).font(iced::Font {
                    weight: iced::font::Weight::Bold,
                    ..iced::Font::with_name(shell::FONT_SANS)
                }),
                txt("·", 10.0, true, false),
                txt(message.timing.as_deref().unwrap_or(""), 9.0, true, false)
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center)]
            .spacing(shell::MESSAGE_PART_GAP);
            for item in &message.parts {
                parts = parts.push(part(item));
            }
            messages = messages.push(
                row![
                    container(txt(&message.avatar, 13.0, false, false).center())
                        .center(24)
                        .width(24)
                        .height(24)
                        .style(|_| frame(color::ACCENT_PALE, color::LIGHT_BORDER, 7.0)),
                    container(parts).width(Length::Fill)
                ]
                .spacing(8)
                .padding(iced::Padding {
                    left: shell::ASSISTANT_AVATAR_INSET - shell::COMPOSER_INSET,
                    right: shell::ASSISTANT_CONTENT_RIGHT - shell::COMPOSER_INSET,
                    ..iced::Padding::ZERO
                }),
            );
        }
    }
    let dated = if conversation.is_fixture {
        column![
            container(txt(FIXTURE_DATE_SEPARATOR, 9.0, true, false))
                .height(12)
                .align_x(iced::Alignment::Center)
                .width(Length::Fill),
            Space::new().height(2),
            messages
        ]
    } else {
        column![messages]
    };
    scrollable(
        container(dated)
            .padding(iced::Padding {
                top: if conversation.is_fixture {
                    0.0
                } else {
                    shell::CONVERSATION_TOP_PADDING - crate::native_shell::CONTENT_TOP
                },
                right: 0.0,
                bottom: 20.0,
                left: 0.0,
            })
            .width(Length::Fill),
    )
    .height(Length::Fill)
    .into()
}

pub(crate) fn inspector_view(
    inspector: &Inspector,
    is_fixture: bool,
) -> Element<'static, AppMessage> {
    let mut content = column![
        row![
            icon(Icon::Agent, rgb(color::CHROME_TEXT), 16.0),
            tooltip(
                txt(elide(&inspector.title, 20), 12.0, false, true).font(iced::Font {
                    weight: iced::font::Weight::Bold,
                    ..iced::Font::with_name(shell::FONT_SANS)
                }),
                txt(&inspector.title, 12.0, false, true),
                tooltip::Position::Left
            ),
            Space::new().width(Length::Fill),
            container(txt(&inspector.badge, 9.0, false, true))
                .padding([3, 6])
                .style(|_| frame(color::NAVIGATION_SELECTED, color::DARK_BORDER, 6.0))
        ]
        .spacing(7)
        .align_y(iced::Alignment::Center),
        tooltip(
            txt(elide(&inspector.subtitle, 44), 10.0, true, true),
            txt(&inspector.subtitle, 10.0, true, true),
            tooltip::Position::Left
        ),
        container(
            row![
                progress_bar(
                    0.0..=inspector.total.max(1) as f32,
                    inspector.completed as f32
                )
                .girth(4)
                .style(|_| iced::widget::progress_bar::Style {
                    background: rgb([44, 58, 80]).into(),
                    bar: rgb(color::ACCENT).into(),
                    border: iced::Border {
                        radius: 2.0.into(),
                        ..Default::default()
                    }
                }),
                txt(
                    if inspector.total == 0 {
                        "No progress".into()
                    } else {
                        format!("{} / {}", inspector.completed, inspector.total)
                    },
                    9.0,
                    true,
                    true
                )
            ]
            .height(12)
            .spacing(8)
            .align_y(iced::Alignment::Center)
        )
        .padding(iced::Padding {
            top: 5.0,
            bottom: 8.0,
            ..iced::Padding::ZERO
        }),
    ]
    .spacing(6);
    let mut metadata = column![];
    for (name, value) in &inspector.metadata {
        let visible: String = value.chars().take(24).collect();
        let visible = if value.chars().count() > 24 {
            format!("{visible}…")
        } else {
            visible
        };
        metadata = metadata.push(
            container(
                row![
                    txt(name.to_uppercase(), 8.0, true, true),
                    Space::new().width(Length::Fill),
                    tooltip(
                        txt(visible, 10.0, false, true),
                        txt(value, 10.0, false, true),
                        tooltip::Position::Left
                    )
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            )
            .height(shell::INSPECTOR_METADATA_ROW_HEIGHT),
        );
    }
    content = content.push(metadata);
    content = content.push(container(txt("CURRENT STEP", 8.0, true, true)).height(12));
    let current = if let Some(step) = &inspector.current_step {
        container(
            column![
                tooltip(
                    txt(elide(&step.title, 43), 10.0, false, true),
                    txt(&step.title, 10.0, false, true),
                    tooltip::Position::Left
                ),
                tooltip(
                    txt(elide(&step.detail, 49), 9.0, true, true),
                    txt(&step.detail, 9.0, true, true),
                    tooltip::Position::Left
                )
            ]
            .spacing(6),
        )
        .padding([7, 9])
        .width(Length::Fill)
        .style(|_| frame(color::NAVIGATION, color::DARK_BORDER, 8.0))
    } else {
        container(txt("No active step", 10.0, true, true))
            .padding(9)
            .width(Length::Fill)
            .style(|_| frame(color::NAVIGATION, color::DARK_BORDER, 8.0))
    };
    content = content.push(current.height(shell::INSPECTOR_CURRENT_HEIGHT));
    content = content.push(txt("TASK QUEUE", 8.0, true, true));
    let mut queue = column![].spacing(shell::INSPECTOR_QUEUE_GAP);
    if inspector.queue.is_empty() {
        queue = queue.push(
            container(txt("No queued tasks", 10.0, true, true))
                .height(shell::INSPECTOR_QUEUE_ROW_HEIGHT),
        );
    }
    for step in &inspector.queue {
        queue = queue.push(step_row(step, true));
    }
    content = content.push(queue).push(Space::new().height(Length::Fill));
    content = content.push(
        row![
            txt(inspector.output_title.to_uppercase(), 8.0, true, true),
            Space::new().width(Length::Fill),
            txt(
                if is_fixture { "Sample" } else { "Observed" },
                9.0,
                true,
                true
            )
        ]
        .spacing(6),
    );
    content = content.push(
        container(scrollable(mono(&inspector.output, 10.0, true)))
            .padding(10)
            .height(shell::INSPECTOR_OUTPUT_HEIGHT)
            .width(Length::Fill)
            .style(|_| frame(color::TERMINAL, color::DARK_BORDER, 8.0)),
    );
    container(
        container(content)
            .padding(iced::Padding {
                top: 13.0,
                right: 13.0,
                bottom: shell::INSPECTOR_OUTPUT_BOTTOM,
                left: 13.0,
            })
            .height(Length::Fill)
            .style(|_| frame(color::INSPECTOR_CARD, color::DARK_BORDER, 12.0)),
    )
    .padding(iced::Padding {
        top: 0.0,
        right: 14.0,
        bottom: 14.0,
        left: 14.0,
    })
    .height(Length::Fill)
    .into()
}

/// Production boundary: only observed native data is mapped. A shell is never
/// relabelled Assistant, and terminal output is never inferred task progress.
pub(crate) fn observed_inspector(app: &Frost) -> Inspector {
    let mut inspector = Inspector::empty();
    if let Some(task) = app
        .task_panel
        .selected
        .and_then(|id| app.task_manager.get(id))
    {
        inspector.title = task.title.clone();
        inspector.badge = task.status.label().into();
        inspector.subtitle = task
            .status_detail
            .clone()
            .unwrap_or_else(|| "Selected task".into());
        inspector.subtitle = format!("{} · {}", inspector.subtitle, task.provider.display_name());
    }
    if app.agent.session.is_some() && inspector.badge == "No task" {
        inspector.badge = if app.agent.loading {
            "Thinking"
        } else {
            "Active"
        }
        .into();
        inspector.subtitle = if app.agent.status.is_empty() {
            "Local agent session".into()
        } else {
            app.agent.status.clone()
        };
        if !app.agent.provider_label.is_empty() {
            inspector.subtitle = format!("{} · {}", inspector.subtitle, app.agent.provider_label);
        }
    }
    inspector.queue = app
        .task_manager
        .tasks()
        .iter()
        .rev()
        .take(5)
        .map(|task| Step {
            title: task.title.clone(),
            detail: task.status.label().into(),
            elapsed: None,
            state: if task.status == crate::agent_task::TaskStatus::Completed {
                StepState::Complete
            } else if task.status.is_running() {
                StepState::Running
            } else {
                StepState::Queued
            },
        })
        .collect();
    if let Some(session) = app.sessions.get(app.active) {
        let private = app
            .tabs
            .get(app.active_tab)
            .is_some_and(|tab| tab.private_title);
        for (name, value) in &mut inspector.metadata {
            match name.as_str() {
                "Model" if app.agent.session.is_some() => *value = app.config.ai_model.clone(),
                "Workspace" => {
                    *value = if private {
                        "Private workspace".into()
                    } else {
                        session
                            .cwd_display()
                            .unwrap_or_else(|| "Local workspace".into())
                    }
                }
                "Branch" if !private => {
                    *value = session
                        .git_meta_cache
                        .as_ref()
                        .map(|meta| meta.branch.clone())
                        .unwrap_or_else(|| "—".into())
                }
                "Environment" => *value = "Local terminal".into(),
                _ => {}
            }
        }
        if let Some(command) = session.terminal.running_command_text() {
            inspector.current_step = Some(Step {
                title: command.chars().take(100).collect(),
                detail: "Observed terminal command".into(),
                state: StepState::Running,
                elapsed: None,
            });
        }
        let output = session
            .projection
            .cells()
            .iter()
            .rev()
            .take(12)
            .collect::<Vec<_>>();
        inspector.output = output
            .into_iter()
            .rev()
            .map(|row| row.iter().take(38).map(|c| c.character).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
    }
    inspector
}
