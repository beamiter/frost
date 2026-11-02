//! Native rich components shared by fixture and observed shell/agent boundaries.
use super::{Message as AppMessage, *};
use crate::glass_profile::{self as shell, color};
use crate::reference_icon::{icon, Icon};
use iced::widget::{column, progress_bar};
use jterm_core::reference_conversation::*;

fn rgb(value: [u8; 3]) -> Color {
    shell::rgba(value, 1.0)
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

/// Keep the retained rich canvas scrollable quiet without changing its rail
/// dimensions, hit area or offset model.
fn conversation_scrollbar_style(
    theme: &iced::Theme,
    status: iced::widget::scrollable::Status,
) -> iced::widget::scrollable::Style {
    use iced::widget::scrollable::Status;
    let alpha = match status {
        Status::Dragged {
            is_vertical_scrollbar_dragged: true,
            ..
        } => 0.5,
        Status::Hovered {
            is_vertical_scrollbar_hovered: true,
            ..
        } => 0.4,
        _ => 0.15,
    };
    let mut style = iced::widget::scrollable::default(theme, status);
    style.vertical_rail.background = None;
    style.vertical_rail.border = iced::Border::default();
    style.vertical_rail.scroller.background = rgb(color::CANVAS_MUTED).scale_alpha(alpha).into();
    style.vertical_rail.scroller.border = iced::Border {
        radius: 2.0.into(),
        ..Default::default()
    };
    style
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

#[derive(Clone, Copy)]
struct TextScale(f32);
impl TextScale {
    fn line_box(self, role: f32, minimum: f32, padding: f32) -> f32 {
        if self.0 <= 1.0 {
            minimum
        } else {
            ui_typography::row_height(role * self.0, minimum, padding)
        }
    }

    fn txt(
        self,
        value: impl Into<String>,
        size: f32,
        dim: bool,
        dark: bool,
    ) -> iced::widget::Text<'static> {
        txt(value, size * self.0, dim, dark)
    }
    fn mono(self, value: impl Into<String>, size: f32, dark: bool) -> iced::widget::Text<'static> {
        mono(value, size * self.0, dark)
    }
}

fn elide(value: &str, max: usize) -> String {
    let short: String = value.chars().take(max).collect();
    if value.chars().count() > max {
        format!("{short}…")
    } else {
        short
    }
}

fn part(value: &Part, fonts: TextScale) -> Element<'static, AppMessage> {
    match value {
        Part::Heading(value) => fonts
            .txt(value, shell::type_size::HEADLINE, false, false)
            .font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..iced::Font::with_name(shell::FONT_SANS)
            })
            .into(),
        Part::Paragraph(value) => fonts
            .txt(value, 12.0, false, false)
            .color(rgb(color::MESSAGE_BODY))
            .line_height(1.5)
            .into(),
        Part::Metadata(values) => row(values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let progress = index == 5;
                let fg = if progress {
                    color::DIFF_ADDED_TEXT
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
                        fonts.txt(value, 9.0, true, false).color(rgb(fg))
                    ]
                    .spacing(4)
                    .align_y(iced::Alignment::Center),
                )
                .padding([2, 6])
                .height(fonts.line_box(9.0, shell::METADATA_PILL_HEIGHT, 4.0))
                .align_y(iced::Alignment::Center)
                .style(move |_| {
                    frame(
                        if progress {
                            color::DIFF_ADDED_BACKGROUND
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
                .map(|step| step_row(step, false, fonts.0))
                .collect::<Vec<_>>();
            container(column(rows).spacing(1))
                .style(|_| frame(color::ACCENT_PALE, color::LIGHT_BORDER, 8.0))
                .into()
        }
        Part::Diff(diff) => diff_view(diff, fonts),
        Part::Terminal {
            title,
            text: output,
        } => container(column![
            container(
                row![
                    icon(Icon::Terminal, rgb(color::CHROME_MUTED), 13.0),
                    fonts.txt(title, 10.0, false, true),
                    Space::new().width(Length::Fill),
                    icon(Icon::ChevronDown, rgb(color::CHROME_MUTED), 12.0)
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center)
            )
            .padding([7, 10]),
            container(fonts.mono(output, 10.0, true)).padding([8, 10]),
        ])
        .width(Length::Fill)
        .style(|_| frame(color::TERMINAL, color::DARK_BORDER, 10.0))
        .into(),
    }
}

fn step_row(step: &Step, dark: bool, font_scale: f32) -> Element<'static, AppMessage> {
    let fonts = TextScale(font_scale);
    let mark = match step.state {
        StepState::Complete => Icon::Check,
        StepState::Running => Icon::Agent,
        StepState::Queued => Icon::More,
        StepState::Attention => Icon::More,
    };
    let state = step.state;
    container(
        row![
            if step.state == StepState::Attention {
                fonts.txt("!", 12.0, false, dark).into()
            } else {
                icon(
                    mark,
                    rgb(if step.state == StepState::Complete {
                        color::DIFF_ADDED_TEXT
                    } else {
                        color::ACCENT
                    }),
                    13.0,
                )
            },
            tooltip(
                container(
                    fonts
                        .txt(
                            elide(&step.title, if dark { 35 } else { 80 }),
                            10.0,
                            false,
                            dark
                        )
                        .wrapping(text::Wrapping::None)
                        .color(rgb(if dark {
                            color::CHROME_TEXT
                        } else {
                            color::MESSAGE_STEP_TEXT
                        }))
                )
                .width(Length::Fill)
                .clip(true),
                fonts
                    .txt(&step.title, 10.0, false, dark)
                    .color(rgb(if dark {
                        color::CHROME_TEXT
                    } else {
                        color::MESSAGE_STEP_TEXT
                    })),
                tooltip::Position::Left
            ),
            fonts.txt(step.elapsed.as_deref().unwrap_or(""), 9.0, true, dark)
        ]
        .spacing(7)
        .align_y(iced::Alignment::Center),
    )
    .padding([5, 9])
    .height(if dark {
        crate::inspector_layout::queue_row_height(font_scale)
    } else {
        fonts.line_box(10.0, shell::TASK_ROW_HEIGHT, 10.0)
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

fn diff_view(diff: &CodeDiff, fonts: TextScale) -> Element<'static, AppMessage> {
    let copied = diff
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let copy = button(
        row![
            icon(Icon::Copy, rgb(color::CANVAS_MUTED), 12.0),
            fonts.txt("Copy", 9.0, true, false)
        ]
        .spacing(4),
    )
    .on_press(AppMessage::ReferenceCopy(copied))
    .padding([3, 6])
    .height(fonts.line_box(9.0, 22.0, 6.0))
    .style(diff_action_style);
    let apply = tooltip(
        button(fonts.txt("Apply", 9.0, true, false))
            .padding([3, 6])
            .height(fonts.line_box(9.0, 22.0, 6.0))
            .style(diff_action_style),
        fonts.txt("No authorized edit target", 10.0, true, false),
        tooltip::Position::Top,
    );
    let header = container(
        row![
            icon(Icon::Code, rgb(color::CANVAS_MUTED), 13.0),
            fonts.txt(&diff.path, 10.0, false, false),
            Space::new().width(Length::Fill),
            fonts.txt(&diff.language, 9.0, true, false),
            fonts.txt(
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
    .height(
        fonts
            .line_box(10.0, shell::DIFF_HEADER_HEIGHT, 8.0)
            .max(fonts.line_box(9.0, 22.0, 6.0) + 8.0),
    )
    .style(|_| frame(color::CONTROL_SURFACE, color::LIGHT_BORDER, 8.0));
    let mut lines = column![];
    for line in &diff.lines {
        let bg = match line.kind {
            DiffKind::Added => color::DIFF_ADDED_BACKGROUND,
            DiffKind::Removed => color::DIFF_REMOVED_BACKGROUND,
            DiffKind::Context => color::TERMINAL,
        };
        let foreground = match line.kind {
            DiffKind::Added => color::DIFF_ADDED_TEXT,
            DiffKind::Removed => color::DIFF_REMOVED_TEXT,
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
                    fonts
                        .mono(
                            line.old_line.map(|n| n.to_string()).unwrap_or_default(),
                            9.0,
                            false
                        )
                        .width(shell::DIFF_GUTTER_COLUMN_WIDTH * fonts.0.max(1.0)),
                    fonts
                        .mono(
                            line.new_line.map(|n| n.to_string()).unwrap_or_default(),
                            9.0,
                            false
                        )
                        .width(shell::DIFF_GUTTER_COLUMN_WIDTH * fonts.0.max(1.0)),
                    fonts
                        .mono(sign, 10.0, false)
                        .color(rgb(foreground))
                        .width(shell::DIFF_SIGN_WIDTH * fonts.0.max(1.0)),
                    fonts
                        .mono(&line.text, 10.0, false)
                        .color(rgb(foreground))
                        .wrapping(text::Wrapping::None)
                ]
                .spacing(0)
                .align_y(iced::Alignment::Center),
            )
            .padding([0, 8])
            .height(fonts.line_box(10.0, shell::DIFF_LINE_HEIGHT, 0.0))
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
    .style(|_| frame(color::TERMINAL, color::LIGHT_BORDER, 10.0))
    .into()
}

pub(crate) fn conversation_view(
    conversation: &Conversation,
    content_font_size: f32,
) -> Element<'static, AppMessage> {
    let fonts = TextScale(ui_typography::reference_role_size(1.0, content_font_size));
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
                        container(
                            fonts
                                .txt(paragraphs, 12.0, false, false)
                                .color(rgb(color::MESSAGE_USER_TEXT))
                        )
                        .padding([8, 13])
                        .width(shell::USER_BUBBLE_WIDTH)
                        .height(if fonts.0 <= 1.0 {
                            Length::Fixed(shell::USER_BUBBLE_HEIGHT)
                        } else {
                            Length::Shrink
                        })
                        .style(|_| frame(
                            color::USER_BUBBLE,
                            color::USER_BUBBLE,
                            10.0
                        )),
                        container(fonts.txt(&message.avatar, 10.0, false, false).center())
                            .center(24)
                            .width(24)
                            .height(24)
                            .style(|_| frame(color::ACCENT_PALE, color::LIGHT_BORDER, 12.0)),
                    ]
                    .spacing(9)
                    .align_y(iced::Alignment::Center),
                )
                .align_left(Length::Fill),
            );
        } else {
            let mut parts = column![row![
                fonts
                    .txt(&message.author, shell::type_size::ROLE, false, false)
                    .font(iced::Font {
                        weight: iced::font::Weight::Bold,
                        ..iced::Font::with_name(shell::FONT_SANS)
                    }),
                fonts.txt("·", 10.0, true, false),
                fonts.txt(message.timing.as_deref().unwrap_or(""), 9.0, true, false)
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center)]
            .spacing(shell::MESSAGE_PART_GAP);
            for item in &message.parts {
                parts = parts.push(part(item, fonts));
            }
            messages = messages.push(
                row![
                    container(fonts.txt(&message.avatar, 13.0, false, false).center())
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
            container(fonts.txt(FIXTURE_DATE_SEPARATOR, 9.0, true, false))
                .height(fonts.line_box(9.0, 12.0, 0.0))
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
    .style(conversation_scrollbar_style)
    .into()
}

pub(crate) fn inspector_view(
    inspector: &Inspector,
    is_fixture: bool,
    chrome_font_size: f32,
    viewport_height: f32,
) -> Element<'static, AppMessage> {
    let fonts = TextScale(ui_typography::reference_role_size(1.4, chrome_font_size));
    let metrics = crate::inspector_layout::Metrics::new(
        fonts.0,
        inspector.metadata.len(),
        inspector.queue.len(),
    );
    let inactive_badge =
        inspector.badge == "No task" || inspector.badge.eq_ignore_ascii_case("Unavailable");
    let mut content = column![
        row![
            icon(Icon::Agent, rgb(color::CHROME_TEXT), 16.0),
            tooltip(
                container(
                    fonts
                        .txt(elide(&inspector.title, 20), 12.0, false, true)
                        .wrapping(text::Wrapping::None)
                        .font(iced::Font {
                            weight: iced::font::Weight::Bold,
                            ..iced::Font::with_name(shell::FONT_SANS)
                        })
                )
                .width(Length::Fill)
                .clip(true),
                fonts.txt(&inspector.title, 12.0, false, true),
                tooltip::Position::Left
            ),
            tooltip(
                container(
                    fonts
                        .txt(
                            &inspector.badge,
                            shell::INSPECTOR_BADGE_FONT_SIZE,
                            inactive_badge,
                            true,
                        )
                        .wrapping(text::Wrapping::None)
                )
                .padding([
                    shell::INSPECTOR_BADGE_VERTICAL_PADDING,
                    shell::INSPECTOR_BADGE_HORIZONTAL_PADDING,
                ])
                .height(metrics.badge)
                .max_width(120.0 * fonts.0.max(1.0))
                .clip(true)
                .align_y(iced::Alignment::Center)
                .style(move |_| {
                    frame(
                        if inactive_badge {
                            color::NAVIGATION
                        } else {
                            color::NAVIGATION_SELECTED
                        },
                        color::DARK_BORDER,
                        shell::INSPECTOR_BADGE_RADIUS,
                    )
                }),
                fonts.txt(&inspector.badge, 9.0, inactive_badge, true),
                tooltip::Position::Left
            )
        ]
        .spacing(7)
        .height(metrics.header)
        .align_y(iced::Alignment::Center),
        tooltip(
            container(
                fonts
                    .txt(elide(&inspector.subtitle, 44), 10.0, true, true)
                    .wrapping(text::Wrapping::None),
            )
            .height(metrics.subtitle),
            fonts.txt(&inspector.subtitle, 10.0, true, true),
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
                fonts.txt(
                    if inspector.total == 0 {
                        shell::NO_PROGRESS_LABEL.into()
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
        .height(metrics.progress)
        .padding(iced::Padding {
            top: 5.0,
            bottom: 8.0,
            ..iced::Padding::ZERO
        }),
    ]
    .spacing(crate::inspector_layout::CONTENT_GAP);
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
                    fonts.txt(name, 8.0, true, true),
                    Space::new().width(Length::Fill),
                    tooltip(
                        fonts.txt(visible, 10.0, false, true),
                        fonts.txt(value, 10.0, false, true),
                        tooltip::Position::Left
                    )
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            )
            .height(metrics.metadata_row),
        );
    }
    content = content.push(metadata);
    content = content
        .push(container(fonts.txt("CURRENT STEP", 9.0, true, true)).height(metrics.current_label));
    let current = if let Some(step) = &inspector.current_step {
        container(
            column![
                tooltip(
                    container(
                        fonts
                            .txt(elide(&step.title, 43), 10.0, false, true)
                            .wrapping(text::Wrapping::None)
                    )
                    .width(Length::Fill)
                    .clip(true),
                    fonts.txt(&step.title, 10.0, false, true),
                    tooltip::Position::Left
                ),
                tooltip(
                    container(
                        fonts
                            .txt(elide(&step.detail, 49), 9.0, true, true)
                            .wrapping(text::Wrapping::None)
                    )
                    .width(Length::Fill)
                    .clip(true),
                    fonts.txt(&step.detail, 9.0, true, true),
                    tooltip::Position::Left
                )
            ]
            .spacing(6),
        )
        .padding([7, 9])
        .width(Length::Fill)
        .style(|_| frame(color::NAVIGATION, color::DARK_BORDER, 8.0))
    } else {
        container(fonts.txt("No active step", 10.0, true, true))
            .padding(9)
            .width(Length::Fill)
            .style(|_| frame(color::NAVIGATION, color::DARK_BORDER, 8.0))
    };
    content = content.push(current.height(metrics.current));
    content = content
        .push(container(fonts.txt("TASK QUEUE", 8.0, true, true)).height(metrics.queue_label));
    let mut queue = column![].spacing(shell::INSPECTOR_QUEUE_GAP);
    if inspector.queue.is_empty() {
        queue = queue.push(
            container(fonts.txt("No queued tasks", 10.0, true, true)).height(metrics.queue_row),
        );
    }
    for step in &inspector.queue {
        queue = queue.push(step_row(step, true, fonts.0));
    }
    content = content.push(queue).push(Space::new().height(Length::Fill));
    content = content.push(
        row![
            fonts.txt(inspector.output_title.to_uppercase(), 8.0, true, true),
            Space::new().width(Length::Fill),
            fonts.txt(
                if is_fixture { "Sample" } else { "Observed" },
                9.0,
                true,
                true
            )
        ]
        .spacing(6)
        .height(metrics.output_label),
    );
    content = content.push(
        container(scrollable(fonts.mono(&inspector.output, 10.0, true)))
            .padding(10)
            .height(metrics.output)
            .width(Length::Fill)
            .style(|_| frame(color::TERMINAL, color::DARK_BORDER, 8.0)),
    );
    container(
        scrollable(
            container(content)
                .padding(iced::Padding {
                    top: crate::inspector_layout::CARD_TOP,
                    right: crate::inspector_layout::CARD_SIDE,
                    bottom: shell::INSPECTOR_OUTPUT_BOTTOM,
                    left: crate::inspector_layout::CARD_SIDE,
                })
                .height(metrics.card_height(viewport_height))
                .style(|_| {
                    frame(
                        color::INSPECTOR_CARD,
                        color::DARK_BORDER,
                        shell::CARD_RADIUS,
                    )
                }),
        )
        .style(conversation_scrollbar_style)
        .height(Length::Fill),
    )
    .padding(iced::Padding {
        top: 0.0,
        right: 12.0,
        bottom: crate::inspector_layout::OUTER_BOTTOM,
        left: 9.0,
    })
    .height(Length::Fill)
    .into()
}

/// Production boundary: only observed native data is mapped. A shell is never
/// relabelled Assistant, and terminal output is never inferred task progress.
pub(crate) fn observed_inspector(app: &Frost) -> Inspector {
    let tasks = if app.config.experimental_task_sidebar {
        app.task_manager.tasks()
    } else {
        &[]
    };
    let inspector = observed_task_inspector(tasks, app.task_panel.selected, |id| {
        crate::agent_task::DashboardActivity {
            has_agent_terminal: app
                .task_terminal_index(id, crate::agent_task::TaskTerminalRole::Agent)
                .is_some(),
            has_validation_terminal: app
                .task_terminal_index(id, crate::agent_task::TaskTerminalRole::Validation)
                .is_some(),
            has_active_agent_stream: app.task_manager.has_active_agent_event_stream(id),
            native_preparing: app.agent_runtime.has_preparing(id),
        }
    });
    private_inspector(
        redact_inspector(inspector, app.config.ai_redact_secrets),
        app.tabs
            .get(app.active_tab)
            .is_some_and(|tab| tab.private_title),
    )
}

fn redact_inspector(mut inspector: Inspector, enabled: bool) -> Inspector {
    if enabled {
        let redact = |text: &mut String| {
            *text = jterm_core::redact::redact_secrets(text);
        };
        redact(&mut inspector.title);
        redact(&mut inspector.subtitle);
        for (_, value) in &mut inspector.metadata {
            redact(value);
        }
        if let Some(step) = &mut inspector.current_step {
            redact(&mut step.title);
            redact(&mut step.detail);
        }
        for step in &mut inspector.queue {
            redact(&mut step.title);
            redact(&mut step.detail);
        }
        redact(&mut inspector.output);
    }
    inspector
}

fn private_inspector(mut inspector: Inspector, private: bool) -> Inspector {
    if private {
        inspector.title = "Private task".into();
        for (name, value) in &mut inspector.metadata {
            if matches!(name.as_str(), "Workspace" | "Branch") {
                *value = "Hidden for private tab".into();
            }
        }
        if let Some(step) = &mut inspector.current_step {
            step.detail = "Open the task workspace to inspect task details.".into();
        }
        for step in &mut inspector.queue {
            step.title = "Private task".into();
        }
        inspector.output = "Task details are hidden for this private tab.".into();
    }
    inspector
}

#[cfg(test)]
mod inspector_tests {
    use super::*;

    #[test]
    fn task_projection_obeys_secret_redaction_preference_everywhere() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let mut inspector = acceptance_fixture().inspector;
        inspector.title = secret.into();
        inspector.subtitle = secret.into();
        inspector.metadata = vec![("Workspace".into(), secret.into())];
        inspector.current_step.as_mut().unwrap().title = secret.into();
        inspector.current_step.as_mut().unwrap().detail = secret.into();
        inspector.queue[0].title = secret.into();
        inspector.queue[0].detail = secret.into();
        inspector.output = secret.into();
        assert_eq!(redact_inspector(inspector.clone(), false), inspector);
        assert!(!format!("{:?}", redact_inspector(inspector, true)).contains(secret));
    }

    #[test]
    fn private_task_projection_hides_paths_titles_and_retained_detail() {
        let inspector = acceptance_fixture().inspector;
        let original = inspector.clone();
        assert_eq!(private_inspector(inspector.clone(), false), original);
        let private = private_inspector(inspector, true);
        assert_eq!(private.title, "Private task");
        assert!(private
            .metadata
            .iter()
            .filter(|(name, _)| matches!(name.as_str(), "Workspace" | "Branch"))
            .all(|(_, value)| value == "Hidden for private tab"));
        assert!(private
            .queue
            .iter()
            .all(|step| step.title == "Private task"));
        assert_eq!(private.completed, original.completed);
        assert_eq!(private.total, original.total);
        assert!(!private.output.contains(&original.output));
        assert_ne!(
            private.current_step.unwrap().detail,
            original.current_step.unwrap().detail
        );
    }
}
