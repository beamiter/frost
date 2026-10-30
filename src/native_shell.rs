//! Native Iced binding for the family's measured workspace contract.
//! The terminal widget remains the sole PTY, selection and IME input owner.

use super::*;
use crate::glass_profile::{self as shell, color};
use crate::reference_icon::{icon as vector_icon, Icon};
use iced::widget::{column, opaque};

pub(crate) const CONTENT_TOP: f32 = crate::native_viewport::CONTENT_TOP;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum InspectorTab {
    #[default]
    Agent,
    Context,
    Files,
}

#[derive(Clone, Debug)]
pub(crate) enum ContextCopy {
    Directory { session_id: usize, expected: String },
    Command { session_id: usize, expected: String },
}

fn context_copy_value(
    expected_session: usize,
    active_session: usize,
    private: bool,
    remote: bool,
    expected: &str,
    current: Option<String>,
) -> Option<String> {
    current.filter(|current| {
        expected_session == active_session && !private && !remote && current == expected
    })
}

fn rgb(value: [u8; 3]) -> Color {
    shell::rgba(value, 1.0)
}

pub(crate) fn composer_border_color() -> Color {
    rgb(color::LIGHT_BORDER).scale_alpha(0.6)
}

pub(crate) fn accent_pill_background() -> iced::Background {
    iced::gradient::Linear::new(iced::Radians::PI)
        .add_stop(0.0, rgb(color::ACCENT).scale_alpha(0.9))
        .add_stop(1.0, rgb(color::ACCENT))
        .into()
}

fn panel(
    theme: &iced::Theme,
    background: [u8; 3],
    border: [u8; 3],
    radius: f32,
) -> container::Style {
    container::Style {
        background: Some(shell::rgba(background, theme.palette().background.a).into()),
        text_color: Some(rgb(color::CHROME_TEXT)),
        border: iced::Border {
            color: shell::rgba(border, theme.palette().background.a),
            width: 1.0,
            radius: radius.into(),
        },
        shadow: iced::Shadow::default(),
        ..Default::default()
    }
}

fn control(
    selected: bool,
    light: bool,
) -> impl Fn(&iced::Theme, button::Status) -> button::Style + 'static {
    move |theme, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let disabled = matches!(status, button::Status::Disabled);
        button::Style {
            background: (selected || hovered || light).then(|| {
                shell::rgba(
                    if selected {
                        if light {
                            color::ACCENT_PALE
                        } else {
                            color::NAVIGATION_SELECTED
                        }
                    } else if hovered {
                        color::ACCENT_PALE
                    } else {
                        color::CONTROL_SURFACE
                    },
                    theme.palette().background.a,
                )
                .into()
            }),
            text_color: rgb(if disabled {
                if light {
                    color::CANVAS_MUTED
                } else {
                    color::CHROME_MUTED
                }
            } else if light {
                color::CANVAS_TEXT
            } else {
                color::CHROME_TEXT
            }),
            border: iced::Border {
                color: if light && (selected || hovered) {
                    rgb(color::CONTROL_BORDER)
                } else {
                    Color::TRANSPARENT
                },
                width: if light && (selected || hovered) {
                    1.0
                } else {
                    0.0
                },
                radius: shell::CONTROL_RADIUS.into(),
            },
            ..Default::default()
        }
    }
}

fn mode_control(selected: bool) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |theme, status| {
        let mut style = control(selected, false)(theme, status);
        style.border = iced::Border {
            color: rgb(color::CONTROL_BORDER),
            width: 1.0,
            radius: shell::CONTROL_RADIUS.into(),
        };
        if !selected {
            style.background =
                Some(shell::rgba(color::CONTROL_SURFACE, theme.palette().background.a).into());
        }
        if selected {
            style.background = Some(accent_pill_background());
            style.border.color = rgb(color::ACCENT).scale_alpha(0.5);
            style.border.width = 1.0;
        }
        style
    }
}

fn label(
    value: impl Into<String>,
    size: f32,
    muted: bool,
    light: bool,
) -> iced::widget::Text<'static> {
    text(value.into())
        .font(iced::Font::with_name(shell::FONT_SANS))
        .size(size)
        .color(rgb(match (muted, light) {
            (true, true) => color::CANVAS_MUTED,
            (false, true) => color::CANVAS_TEXT,
            (true, false) => color::CHROME_MUTED,
            (false, false) => color::CHROME_TEXT,
        }))
}

fn short(value: &str, count: usize) -> String {
    let mut result: String = value.chars().take(count).collect();
    if value.chars().count() > count {
        result.push('…');
    }
    result
}

fn assistant_state_label(state: jterm_core::agent::AgentState) -> &'static str {
    use jterm_core::agent::AgentState;
    match state {
        AgentState::Ready => "Ready for a task",
        AgentState::AwaitingModel => "Waiting for the model",
        AgentState::AwaitingApproval { .. } => "Command awaiting your approval",
        AgentState::AwaitingObservation { .. } => "Approved command running",
        AgentState::Completed => "Task completed",
        AgentState::Cancelled => "Session cancelled",
        AgentState::TurnLimitReached => "Turn limit reached",
    }
}

/// Private tab titles also suppress details and clipboard actions in Context.
fn context_command(private: bool, command: Option<&str>) -> Option<String> {
    (!private)
        .then_some(command)
        .flatten()
        .filter(|command| !command.trim().is_empty())
        .map(|command| jterm_core::review_input::safe_inline_display(command, 4096))
}

fn context_exit_label(exit: Option<i32>, duration_ms: Option<u64>) -> String {
    let status = match exit {
        Some(0) => "Last command succeeded · exit 0".into(),
        Some(code) => format!("Last command exited with {code}"),
        None => "No recorded command exit".into(),
    };
    match duration_ms {
        Some(duration) => format!("{status} · {:.2}s", duration as f64 / 1000.0),
        None => status,
    }
}

impl Frost {
    fn chrome_label(
        &self,
        value: impl Into<String>,
        size: f32,
        muted: bool,
        light: bool,
    ) -> iced::widget::Text<'static> {
        label(
            value,
            ui_typography::reference_role_size(size * 1.5, self.chrome_font_size()),
            muted,
            light,
        )
    }
    fn chrome_row_height(&self, roles: f32, minimum: f32, padding: f32) -> f32 {
        if self.chrome_font_size() <= 14.0 {
            minimum
        } else {
            ui_typography::row_height(
                ui_typography::reference_role_size(roles * 1.5, self.chrome_font_size()),
                minimum,
                padding,
            )
        }
    }
    fn chrome_section(&self, value: &'static str) -> Element<'static, Message> {
        container(self.chrome_label(value, 9.0, true, false))
            .padding([4, 0])
            .into()
    }

    pub(crate) fn reference_layout(&self) -> shell::Layout {
        shell::Layout::with_panels(
            self.win_size.width,
            self.win_size.height,
            self.config.sidebar_width,
            self.config.inspector_width,
            self.sidebar_open,
            self.reference_inspector_open,
        )
        .with_chrome_font_size(self.chrome_font_size())
    }

    pub(crate) fn reference_inspector_width(&self) -> f32 {
        self.reference_layout().inspector.map_or_else(
            || {
                self.config
                    .inspector_width
                    .and_then(crate::glass_layout::inspector_width)
                    .unwrap_or(shell::INSPECTOR_WIDTH)
                    .min(
                        (self.win_size.width
                            - crate::glass_shell_tokens::OUTER_LEFT
                            - crate::glass_shell_tokens::OUTER_RIGHT
                            - crate::glass_shell_tokens::INSPECTOR_GAP)
                            .max(0.0),
                    )
            },
            |panel| panel.width,
        )
    }

    fn reference_resize_grip(
        &self,
        side: crate::sidebar_resize::Side,
        width: f32,
    ) -> Element<'_, Message> {
        crate::sidebar_resize::grip(
            width,
            self.sidebar_resize.is_some_and(|drag| drag.side == side),
            self.c_accent(),
            self.c_border(),
            move |action| Message::SidebarResize(side, action),
        )
    }

    fn reference_private_title(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.private_title)
    }

    fn reference_cwd(&self) -> String {
        if self.reference_private_title() {
            return "Private workspace".into();
        }
        self.sessions
            .get(self.active)
            .and_then(|session| {
                if session.managed_remote {
                    Some("Remote terminal".into())
                } else {
                    session.cwd_display()
                }
            })
            .unwrap_or_else(|| "Local workspace".into())
    }

    pub(crate) fn reference_theme(config: &Config) -> Theme {
        let mut theme = Theme::get_theme(&config.theme).unwrap_or_default();
        // Custom terminal palettes stay under user control. The default shell
        // combines dark chrome with the light conversation surface.
        if matches!(config.theme.as_str(), "dark" | "light") {
            theme.terminal.background = color::TERMINAL;
            theme.terminal.foreground = color::CHROME_TEXT;
            theme.terminal.cursor = color::CHROME_TEXT;
        }
        theme.ui.window_bg = color::CANVAS;
        theme.ui.panel_bg = color::INSPECTOR_CARD;
        theme.ui.text = color::CHROME_TEXT;
        theme.ui.text_disabled = color::CHROME_MUTED;
        theme.ui.border = color::DARK_BORDER;
        theme.tabbar.bg = color::TITLEBAR;
        theme.tabbar.active_border = color::ACCENT;
        theme
    }

    fn shell_button(
        &self,
        value: impl Into<String>,
        action: Message,
        selected: bool,
        light: bool,
    ) -> iced::widget::Button<'static, Message> {
        button(self.chrome_label(value, 11.0, false, light))
            .on_press(action)
            .height(crate::glass_layout::chrome_control_height(
                self.chrome_font_size(),
            ))
            .padding([4, 8])
            .style(control(selected, light))
    }

    fn vector_button(
        &self,
        glyph: Icon,
        action: Message,
        selected: bool,
    ) -> iced::widget::Button<'static, Message> {
        button(container(vector_icon(glyph, rgb(color::CHROME_TEXT), 18.0)).center(32))
            .on_press(action)
            .width(32)
            .height(32)
            .padding(0)
            .style(control(selected, true))
    }

    pub(crate) fn reference_topbar(&self) -> Element<'_, Message> {
        let compact = self.win_size.width < 560.0 || self.chrome_font_size() > 24.0;
        let window_control = |glyph: &'static str, action: Message| {
            button(container(label(glyph, 16.0, false, false)).center(28))
                .on_press(action)
                .width(28)
                .height(28)
                .padding(0)
                .style(control(false, false))
        };
        let controls = row![
            window_control("×", Message::WindowClose),
            window_control("−", Message::WindowMinimize),
            window_control("□", Message::WindowToggleMaximize),
        ]
        .spacing(0)
        .width(84.0)
        .align_y(iced::Alignment::Center);
        let identity = row![
            crate::reference_icon::identity("frost", 22.0),
            self.chrome_label(
                if self.win_size.width >= 850.0 && !compact {
                    "Frost"
                } else {
                    ""
                },
                10.667,
                false,
                false
            ),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        let path = self.reference_cwd();
        let path: Element<'_, Message> = if self.win_size.width >= 1050.0 && !compact {
            tooltip(
                container(self.chrome_label(
                    if self.config.is_chat_mode() {
                        "Chat"
                    } else {
                        "Block"
                    },
                    10.667,
                    true,
                    false,
                ))
                .width(Length::Fixed(90.0))
                .clip(true),
                self.chrome_label(path, 11.0, false, false),
                tooltip::Position::Bottom,
            )
            .into()
        } else {
            Space::new().into()
        };
        let search: Element<'_, Message> = if self.win_size.width >= 850.0 && !compact {
            button(
                row![
                    vector_icon(Icon::Search, rgb(color::CHROME_MUTED), 16.0),
                    self.chrome_label("Search or run a command…", 10.0, true, false),
                    Space::new().width(Length::Fill),
                    container(self.chrome_label("Ctrl ⇧ P", 9.0, true, false))
                        .padding([2, 4])
                        .style(|theme| panel(theme, color::TITLEBAR, color::DARK_BORDER, 4.0))
                ]
                .align_y(iced::Alignment::Center),
            )
            .on_press(Message::ReferencePalette)
            .width(Length::Fixed(if self.win_size.width >= 1500.0 {
                shell::SEARCH_WIDTH
            } else if self.win_size.width >= 1250.0 {
                320.0
            } else {
                230.0
            }))
            .height(crate::glass_layout::chrome_control_height(
                self.chrome_font_size(),
            ))
            .padding([3, 9])
            .style(|theme, status| {
                let mut style = control(false, false)(theme, status);
                style.background = Some(
                    rgb(if matches!(status, button::Status::Hovered) {
                        color::INSPECTOR_CARD
                    } else {
                        color::NAVIGATION
                    })
                    .into(),
                );
                style.border = iced::Border {
                    color: rgb(color::DARK_BORDER),
                    width: 1.0,
                    radius: shell::CONTROL_RADIUS.into(),
                };
                style
            })
            .into()
        } else {
            self.vector_button(Icon::Search, Message::ReferencePalette, false)
                .into()
        };
        let mode = self.config.effective_terminal_mode();
        let search_width = if compact {
            32.0
        } else if self.win_size.width >= 1500.0 {
            shell::SEARCH_WIDTH
        } else if self.win_size.width >= 1250.0 {
            320.0
        } else if self.win_size.width >= 850.0 {
            230.0
        } else {
            32.0
        };
        let search_x = (self.win_size.width - search_width) * 0.5 + shell::SEARCH_CENTER_OFFSET;
        let mode_width = shell::MODE_CONTROL_MIN_WIDTH * (self.chrome_font_size() / 14.0).max(1.0);
        let right_minimum = (if compact { 32.0 } else { mode_width * 2.0 })
            + 32.0
            + if self.win_size.width >= 500.0 {
                28.0 + 4.0 * shell::METADATA_GAP
            } else {
                3.0 * shell::METADATA_GAP
            };
        let left_width = (search_x - 14.0)
            .max(106.0)
            .min((self.win_size.width - search_width - right_minimum - 28.0).max(106.0));
        let identity: Element<'_, Message> = if self.win_size.width >= 560.0 {
            identity.into()
        } else {
            Space::new().into()
        };
        let left = row![controls, identity, path]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .width(left_width);
        let mode_button = |caption, mode, selected| {
            button(container(self.chrome_label(caption, 11.0, false, false)).center_x(Length::Fill))
                .on_press(Message::SetTerminalMode(mode))
                .width(mode_width)
                .height(crate::glass_layout::chrome_control_height(
                    self.chrome_font_size(),
                ))
                .padding([4, 8])
                .style(mode_control(selected))
        };
        let mut right = row![Space::new().width(Length::Fill)]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .width(Length::Fill);
        if !compact {
            right = right
                .push(mode_button(
                    "Block",
                    config::TerminalMode::Block,
                    mode != config::TerminalMode::Chat,
                ))
                .push(mode_button(
                    "Chat",
                    config::TerminalMode::Chat,
                    mode == config::TerminalMode::Chat,
                ));
        } else {
            let (caption, target) = if mode == config::TerminalMode::Chat {
                ("Chat", config::TerminalMode::Block)
            } else {
                ("Block", config::TerminalMode::Chat)
            };
            right = right.push(tooltip(
                self.vector_button(
                    if mode == config::TerminalMode::Chat {
                        Icon::from_key("conversation")
                    } else {
                        Icon::Terminal
                    },
                    Message::SetTerminalMode(target),
                    true,
                ),
                self.chrome_label(
                    format!("{caption} · Switch Block / Chat"),
                    10.0,
                    false,
                    false,
                ),
                tooltip::Position::Bottom,
            ));
        }
        if self.win_size.width >= 500.0 {
            right =
                right.push(self.vector_button(Icon::Settings, Message::ToggleConfigPanel, false));
        }
        let inspector_visible = self.reference_inspector_visible();
        right = right.push(tooltip(
            self.vector_button(
                Icon::PanelRight,
                Message::ReferenceInspectorToggle,
                inspector_visible,
            ),
            self.chrome_label(
                if inspector_visible {
                    "Hide right sidebar"
                } else {
                    "Show right sidebar"
                },
                10.0,
                false,
                false,
            ),
            tooltip::Position::Bottom,
        ));
        let bar = row![left, search, right]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .width(Length::Fill);
        container(
            mouse_area(bar)
                .on_press(Message::WindowDrag)
                .on_double_click(Message::WindowToggleMaximize),
        )
        .padding([0, 8])
        .height(self.reference_layout().titlebar.height)
        .align_y(iced::Alignment::Center)
        .style(|theme| {
            panel(
                theme,
                color::TITLEBAR,
                color::DARK_BORDER,
                shell::CARD_RADIUS,
            )
        })
        .into()
    }

    fn reference_rail(&self) -> Element<'_, Message> {
        let entry = |glyph: Icon,
                     hint: &'static str,
                     action: Message,
                     selected: bool|
         -> Element<'_, Message> {
            tooltip(
                button(container(vector_icon(glyph, rgb(color::CHROME_TEXT), 18.0)).center(32))
                    .on_press(action)
                    .width(32)
                    .height(32)
                    .padding(0)
                    .style(control(selected, false)),
                self.chrome_label(hint, 11.0, false, false),
                tooltip::Position::Right,
            )
            .into()
        };
        let mut rail = column![].spacing(6).align_x(iced::Alignment::Center);
        for (key, title) in jterm_core::reference_conversation::RAIL_NAVIGATION {
            let (action, selected) = match key {
                "workspace" => (Message::ToggleSidebar, self.sidebar_open),
                "folder" => (
                    Message::ReferenceInspector(InspectorTab::Files),
                    self.reference_inspector_tab == InspectorTab::Files,
                ),
                "terminal" => (Message::ReferenceFocusTerminal, false),
                "agent" => (Message::TaskPanelToggle, self.tasks_surface_visible()),
                "search" => (
                    Message::ReferenceCommand(keybindings::Command::SearchOpen),
                    false,
                ),
                _ => unreachable!("shared rail key"),
            };
            let title = if key == "workspace" {
                if self.sidebar_open {
                    "Hide left sidebar"
                } else {
                    "Show left sidebar"
                }
            } else {
                title
            };
            rail = rail.push(entry(Icon::from_key(key), title, action, selected));
        }
        let initial = std::env::var("USER")
            .unwrap_or_else(|_| "User".into())
            .chars()
            .next()
            .unwrap_or('U')
            .to_uppercase()
            .collect::<String>();
        container(
            rail.push(Space::new().height(Length::Fill))
                .push(entry(
                    Icon::Settings,
                    "Settings",
                    Message::ToggleConfigPanel,
                    false,
                ))
                .push(
                    container(label(initial, 14.0, false, false).center())
                        .width(shell::PROFILE_DIAMETER)
                        .height(shell::PROFILE_DIAMETER)
                        .center(shell::PROFILE_DIAMETER)
                        .style(|theme| {
                            panel(
                                theme,
                                color::PROFILE_BACKGROUND,
                                color::PROFILE_BORDER,
                                shell::PROFILE_DIAMETER / 2.0,
                            )
                        }),
                ),
        )
        .padding(iced::Padding {
            top: 6.0,
            right: 0.0,
            bottom: 6.0,
            left: 0.0,
        })
        .width(self.reference_layout().rail.width)
        .align_x(iced::Alignment::Center)
        .height(Length::Fill)
        .style(|theme| panel(theme, color::RAIL, color::DARK_BORDER, shell::CARD_RADIUS))
        .into()
    }

    fn reference_navigation(&self) -> Element<'_, Message> {
        use jterm_core::reference_conversation as model;
        let fixture = model::fixture_enabled();
        let cwd = self.reference_cwd();
        let workspace = std::path::Path::new(&cwd)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&cwd);
        let actual_branch = self
            .sessions
            .get(self.active)
            .filter(|session| !self.reference_private_title() && !session.managed_remote)
            .and_then(|session| session.git_meta_cache.as_ref())
            .map(|meta| meta.branch.as_str());
        let branch_metadata = if fixture {
            model::acceptance_fixture()
                .inspector
                .metadata
                .into_iter()
                .find(|(key, _)| key == "Branch")
                .map(|(_, value)| value)
                .unwrap_or_else(|| "main · sample".into())
        } else {
            actual_branch.unwrap_or("Local").to_owned()
        };
        let branch = if fixture {
            branch_metadata.split_whitespace().next().unwrap_or("main")
        } else {
            &branch_metadata
        };
        let branch_slot: Element<'_, Message> = if fixture || actual_branch.is_some() {
            row![
                vector_icon(Icon::Branch, rgb(color::CHROME_MUTED), 10.0),
                self.chrome_label(short(branch, 6), 8.0, true, false)
                    .wrapping(text::Wrapping::None)
            ]
            .spacing(3)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            self.chrome_label("Local", 8.0, true, false).into()
        };
        let selector = tooltip(
            button(
                row![
                    vector_icon(Icon::Files, rgb(color::CHROME_MUTED), 14.0),
                    container(
                        self.chrome_label(
                            short(
                                if fixture {
                                    "Canvas workspace"
                                } else {
                                    workspace
                                },
                                12
                            ),
                            11.0,
                            false,
                            false
                        )
                        .width(Length::Fill)
                        .wrapping(text::Wrapping::None)
                    )
                    .width(Length::Fill)
                    .clip(true),
                    container(branch_slot).width(49).clip(true),
                    vector_icon(Icon::ChevronDown, rgb(color::CHROME_MUTED), 13.0)
                ]
                .spacing(7)
                .align_y(iced::Alignment::Center),
            )
            .on_press(Message::ReferenceWorkspaceToggle)
            .height(crate::glass_layout::chrome_control_height(
                self.chrome_font_size(),
            ))
            .padding([4, 6])
            .style(control(false, false)),
            self.chrome_label(
                format!("Switch workspace tab · {cwd} · {branch_metadata}"),
                10.0,
                false,
                false,
            ),
            tooltip::Position::Right,
        );
        let mut destinations = column![].spacing(shell::NAVIGATION_ROW_GAP);
        for (index, (key, title)) in model::NAVIGATION.iter().enumerate() {
            let action = match *key {
                "conversation" | "terminal" => Message::ReferenceFocusTerminal,
                "folder" => Message::ReferenceInspector(InspectorTab::Files),
                "agent" => {
                    if self.config.experimental_task_sidebar {
                        Message::TaskPanelToggle
                    } else {
                        Message::ReferenceCommand(keybindings::Command::AgentToggle)
                    }
                }
                _ => Message::ToggleConfigPanel,
            };
            let title = if *key == "conversation" {
                if self.config.is_chat_mode() {
                    "Chat"
                } else {
                    "Block"
                }
            } else {
                title
            };
            destinations = destinations.push(tooltip(
                button(
                    row![
                        vector_icon(Icon::from_key(key), rgb(color::CHROME_MUTED), 15.0),
                        container(
                            self.chrome_label(title, 10.0, false, false)
                                .width(Length::Fill)
                                .wrapping(text::Wrapping::None)
                        )
                        .width(Length::Fill)
                        .clip(true)
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
                )
                .on_press(action)
                .height(self.chrome_row_height(10.0, shell::NAVIGATION_ROW_HEIGHT, 10.0))
                .width(Length::Fill)
                .padding([5, 8])
                .style(control(index == 0, false)),
                self.chrome_label(title, 10.0, false, false),
                tooltip::Position::Right,
            ));
        }
        let mut recent = column![].spacing(3);
        if fixture {
            for item in model::fixture_recent_entries() {
                recent = recent.push(
                    container(
                        row![
                            vector_icon(
                                Icon::from_key(item.icon_key),
                                rgb(color::CHROME_MUTED),
                                13.0
                            ),
                            column![
                                tooltip(
                                    container(
                                        self.chrome_label(&item.title, 10.0, false, false)
                                            .wrapping(text::Wrapping::None)
                                    )
                                    .width(Length::Fill)
                                    .clip(true),
                                    self.chrome_label(item.title, 10.0, false, false),
                                    tooltip::Position::Right
                                ),
                                self.chrome_label(item.detail, 8.0, true, false)
                                    .wrapping(text::Wrapping::None)
                            ]
                            .spacing(1)
                            .width(Length::Fill),
                            self.chrome_label(item.time, 8.0, true, false)
                        ]
                        .spacing(7)
                        .align_y(iced::Alignment::Center),
                    )
                    .height(self.chrome_row_height(18.0, shell::RECENT_ROW_HEIGHT.max(43.0), 7.0))
                    .width(Length::Fill)
                    .clip(true)
                    .padding([3, 7])
                    .style(move |theme| {
                        panel(
                            theme,
                            if item.selected {
                                color::NAVIGATION_SELECTED
                            } else {
                                color::NAVIGATION
                            },
                            color::NAVIGATION,
                            6.0,
                        )
                    }),
                );
            }
        } else {
            for (index, tab) in self.tabs.iter().enumerate() {
                let active = index == self.active_tab;
                let item = container(
                    row![
                        vector_icon(Icon::Terminal, rgb(color::CHROME_MUTED), 14.0),
                        container(
                            self.chrome_label(self.tab_label(index), 10.0, false, false)
                                .width(Length::Fill)
                                .wrapping(text::Wrapping::None)
                        )
                        .width(Length::Fill)
                        .clip(true)
                    ]
                    .spacing(7),
                )
                .height(self.chrome_row_height(10.0, shell::RECENT_ROW_HEIGHT, 14.0))
                .width(Length::Fill)
                .clip(true)
                .padding([7, 8])
                .style(move |theme| {
                    panel(
                        theme,
                        if active {
                            color::NAVIGATION_SELECTED
                        } else {
                            color::NAVIGATION
                        },
                        color::NAVIGATION,
                        7.0,
                    )
                });
                recent = recent.push(tooltip(
                    mouse_area(item)
                        .on_press(Message::TabDragStart(tab.id))
                        .on_release(Message::TabDragEnd(tab.id))
                        .on_right_press(Message::TabMenuOpen(tab.id)),
                    self.chrome_label(self.tab_label(index), 11.0, false, false),
                    tooltip::Position::Right,
                ));
            }
        }
        let mut spaces = column![].spacing(3);
        for (name, description) in model::SPACES.iter() {
            let selected = *name == "Frost";
            let description = if fixture || selected {
                *description
            } else {
                "Unavailable here"
            };
            let labels = column![
                self.chrome_label(*name, 10.0, false, false)
                    .width(Length::Fill)
                    .wrapping(text::Wrapping::None),
                self.chrome_label(description, 8.0, true, false)
                    .width(Length::Fill)
                    .wrapping(text::Wrapping::None)
            ]
            .spacing(3)
            .width(Length::Fill);
            spaces = spaces.push(tooltip(
                container(
                    row![
                        crate::reference_icon::identity(name, 18.0),
                        container(labels).width(Length::Fill).clip(true),
                    ]
                    .spacing(9)
                    .align_y(iced::Alignment::Center),
                )
                .height(self.chrome_row_height(18.0, shell::SPACES_ROW_HEIGHT, 7.0))
                .width(Length::Fill)
                .clip(true)
                .padding([2, 6]),
                self.chrome_label(format!("{name} · {description}"), 10.0, false, false),
                tooltip::Position::Right,
            ));
        }
        let navigation = column![
            Space::new().height(shell::NAVIGATION_LABEL_TOP - shell::TITLEBAR_HEIGHT),
            container(self.chrome_label("WORKSPACE", 9.0, true, false)).height(
                self.chrome_row_height(
                    9.0,
                    shell::WORKSPACE_SELECTOR_TOP - shell::NAVIGATION_LABEL_TOP,
                    2.0
                )
            ),
            selector,
            Space::new().height(
                shell::NAVIGATION_ITEMS_TOP
                    - shell::WORKSPACE_SELECTOR_TOP
                    - shell::WORKSPACE_SELECTOR_HEIGHT
            ),
            destinations,
            Space::new().height(8),
            self.chrome_section("TABS"),
            recent,
            Space::new().height(7),
            self.chrome_section("SPACES"),
            spaces
        ];
        let status = self
            .sessions
            .get(self.active)
            .map(|session| {
                if session.transcript_read_only() {
                    "Session exited"
                } else if session.terminal.is_command_running() {
                    "Command running"
                } else {
                    "Local shell"
                }
            })
            .unwrap_or("No session");
        let footer: Element<'_, Message> = if self.chrome_font_size() <= 24.0 {
            tooltip(
                container(
                    column![
                        container(Space::new().height(1))
                            .width(Length::Fill)
                            .style(|_| container::Style {
                                background: Some(rgb(color::DARK_BORDER).into()),
                                ..Default::default()
                            }),
                        Space::new().height(shell::NAVIGATION_PADDING - 1.0),
                        row![
                            container(Space::new())
                                .width(5)
                                .height(5)
                                .style(|theme| panel(
                                    theme,
                                    color::CHROME_MUTED,
                                    color::CHROME_MUTED,
                                    3.0
                                )),
                            container(
                                self.chrome_label("Local session", 10.0, false, false)
                                    .width(Length::Fill)
                                    .wrapping(text::Wrapping::None)
                            )
                            .width(Length::Fill)
                            .clip(true)
                        ]
                        .spacing(7)
                        .align_y(iced::Alignment::Center),
                        self.chrome_label(status, 8.0, true, false)
                            .width(Length::Fill)
                            .wrapping(text::Wrapping::None)
                    ]
                    .spacing(0),
                )
                .height(
                    (ui_typography::reference_role_size(27.0, self.chrome_font_size()) * 1.3
                        + shell::NAVIGATION_PADDING)
                        .ceil()
                        .max(shell::NAVIGATION_STATUS_HEIGHT),
                )
                .width(Length::Fill)
                .clip(true),
                self.chrome_label(format!("Local session · {status}"), 10.0, false, false),
                tooltip::Position::Right,
            )
            .into()
        } else {
            // The terminal and navigation already expose this session; omit
            // the duplicate status when large text would consume the sidebar.
            Space::new().height(0).into()
        };
        container(column![scrollable(navigation).height(Length::Fill), footer,])
            .padding([0.0, shell::NAVIGATION_PADDING])
            .width(
                self.reference_layout()
                    .navigation
                    .map_or(0.0, |nav| nav.width),
            )
            .height(Length::Fill)
            .style(|theme| {
                panel(
                    theme,
                    color::NAVIGATION,
                    color::DARK_BORDER,
                    shell::CARD_RADIUS,
                )
            })
            .into()
    }

    fn reference_header(&self) -> Element<'_, Message> {
        let layout = self.reference_layout();
        let name = if jterm_core::reference_conversation::fixture_enabled() {
            jterm_core::reference_conversation::acceptance_fixture().title
        } else {
            self.tab_label(self.active_tab)
        };
        let full_title = if name.starts_with("Workspace / ") {
            name.clone()
        } else {
            format!("Workspace / {name}")
        };
        // A single session title shares the row with its existing actions.
        // Truncate only the visible label; the complete title stays in a tooltip.
        let available = (layout.header.width
            - 2.0 * shell::COMPOSER_INSET
            - 3.0 * (shell::HEADER_ACTION_SIZE + shell::HEADER_ACTION_GAP))
            .max(0.0);
        let title_size = ui_typography::reference_role_size(15.0, self.chrome_font_size());
        let title_chars = (available / (title_size * 0.6).max(1.0)).floor() as usize;
        let heading = self
            .chrome_label(
                short(
                    if layout.header.width < 600.0 {
                        &name
                    } else {
                        &full_title
                    },
                    title_chars.saturating_sub(1),
                ),
                10.0,
                false,
                true,
            )
            .width(Length::Fill)
            .wrapping(text::Wrapping::None)
            .font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..iced::Font::with_name(shell::FONT_SANS)
            });
        let heading = tooltip(
            container(heading).width(Length::Fill).clip(true),
            self.chrome_label(full_title, 10.0, false, true),
            iced::widget::tooltip::Position::Bottom,
        )
        .style(self.popup_style());
        let action = |icon, message| {
            button(
                container(vector_icon(icon, rgb(color::CANVAS_MUTED), 16.0))
                    .center(shell::HEADER_ACTION_SIZE),
            )
            .on_press(message)
            .width(shell::HEADER_ACTION_SIZE)
            .height(shell::HEADER_ACTION_SIZE)
            .padding(0)
            .style(control(false, true))
        };
        container(
            row![
                heading,
                action(Icon::Plus, Message::NewSession),
                action(
                    Icon::Search,
                    Message::ReferenceCommand(keybindings::Command::SearchOpen)
                ),
                action(Icon::More, Message::ReferencePalette),
            ]
            .spacing(shell::HEADER_ACTION_GAP)
            .align_y(iced::Alignment::Center),
        )
        .height(layout.header.height)
        .padding([2.0, shell::COMPOSER_INSET])
        .align_y(iced::Alignment::Center)
        .into()
    }

    pub(crate) fn reference_tools_height(&self) -> f32 {
        self.reference_layout().composer.height
    }

    fn reference_tools(&self) -> Element<'_, Message> {
        let font_scale = (self.chrome_font_size() / 14.0).max(1.0);
        let control_height = crate::glass_layout::chrome_control_height(self.chrome_font_size());
        // Use the rendered composer's inner surface, including sidebar toggles,
        // rather than the original layout's cached composer rectangle.
        let composer_width = self.reference_layout().composer.width;
        let available = composer_width / font_scale;
        let tool = |glyph: Icon,
                    title: &'static str,
                    width: f32,
                    action: Message|
         -> Element<'_, Message> {
            let fg = color::CANVAS_MUTED;
            let icon_only = title == jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[4]
                && composer_width
                    < shell::COMPOSER_CONTROL_LEFT
                        + shell::COMPOSER_CONTROL_RIGHT
                        + shell::COMPOSER_PLUS_WIDTH
                        + shell::COMPOSER_SEND_WIDTH
                        + width * font_scale
                        + 2.0 * shell::METADATA_GAP;
            let mut contents = row![]
                .spacing(jterm_core::reference_popup::FOOTER_ICON_LABEL_GAP)
                .align_y(iced::Alignment::Center);
            contents = contents.push(vector_icon(glyph, rgb(fg), 18.0));
            let caption = self
                .chrome_label(
                    title,
                    jterm_core::reference_popup::FOOTER_LABEL_SIZE,
                    false,
                    true,
                )
                .color(rgb(fg))
                .wrapping(text::Wrapping::None);
            if !icon_only {
                contents = contents.push(caption);
                if title == jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[4] {
                    contents = contents.push(vector_icon(Icon::ChevronDown, rgb(fg), 18.0));
                }
            }
            let button = button(contents)
                .on_press(action)
                .height(control_height)
                .width(if icon_only { 32.0 } else { width * font_scale })
                .padding([4.0, jterm_core::reference_popup::FOOTER_HORIZONTAL_PADDING])
                .style(move |theme, status| {
                    let mut style = control(false, true)(theme, status);
                    style.background = Some(
                        rgb(
                            if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                                color::ACCENT_PALE
                            } else {
                                color::CONTROL_SURFACE
                            },
                        )
                        .into(),
                    );
                    if title == jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[3] {
                        style.background = Some(accent_pill_background());
                        style.text_color = rgb(color::CHROME_TEXT);
                    } else {
                        style.text_color = rgb(fg);
                    }
                    style.border = iced::Border {
                        color: rgb(color::CONTROL_BORDER),
                        width: 0.0,
                        radius: shell::CONTROL_RADIUS.into(),
                    };
                    style
                });
            if icon_only {
                tooltip(
                    button,
                    self.chrome_label(title, 10.0, false, true),
                    tooltip::Position::Top,
                )
                .into()
            } else {
                button.into()
            }
        };
        let mut tools = row![button(
            container(vector_icon(Icon::Plus, rgb(color::CANVAS_TEXT), 18.0))
                .center(shell::COMPOSER_PLUS_WIDTH)
        )
        .on_press(Message::NewSession)
        .width(shell::COMPOSER_PLUS_WIDTH)
        .height(control_height)
        .padding(0)
        .style(control(false, true))]
        .spacing(shell::METADATA_GAP)
        .align_y(iced::Alignment::Center);
        if available >= shell::COMPOSER_TERMINAL_MIN_AVAILABLE {
            tools = tools.push(tool(
                Icon::Terminal,
                jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[3],
                shell::COMPOSER_TERMINAL_WIDTH,
                Message::ReferenceFocusTerminal,
            ));
        }
        tools = tools.push(tool(
            Icon::Grid,
            jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[4],
            shell::COMPOSER_TOOLS_WIDTH,
            Message::ReferenceToolsToggle,
        ));
        let mut controls = row![]
            .spacing(shell::METADATA_GAP)
            .align_y(iced::Alignment::Center);
        let action = self.sessions.get(self.active).and_then(|session| {
            let kind = if session.terminal.is_command_running() {
                chat_mode::ControlKind::Interrupt
            } else {
                chat_mode::ControlKind::Submit
            };
            session
                .enabled_chat_control(kind, self.chat_input_ready(session.id))
                .map(|token| match kind {
                    chat_mode::ControlKind::Submit => Message::ChatSubmit(token),
                    chat_mode::ControlKind::Interrupt => Message::ChatInterrupt(token),
                })
        });
        let enabled = action.is_some();
        let running = self
            .sessions
            .get(self.active)
            .is_some_and(|session| session.terminal.is_command_running());
        let arrow = container(vector_icon(
            if running { Icon::Close } else { Icon::Send },
            rgb(color::CHROME_TEXT),
            18.0,
        ))
        .center(shell::COMPOSER_SEND_WIDTH)
        .width(shell::COMPOSER_SEND_WIDTH)
        .height(control_height)
        .style(move |theme| {
            panel(
                theme,
                if enabled {
                    color::ACCENT
                } else {
                    color::LIGHT_BORDER
                },
                color::LIGHT_BORDER,
                7.0,
            )
        });
        controls = controls.push(tooltip(
            chat_mode::press_control(arrow, action),
            self.chrome_label(
                if !enabled {
                    "Use the terminal keyboard"
                } else if running {
                    "Stop command"
                } else {
                    "Send input"
                },
                10.0,
                false,
                true,
            ),
            tooltip::Position::Top,
        ));
        let tools = row![tools.width(Length::Fill), controls]
            .spacing(shell::METADATA_GAP)
            .align_y(iced::Alignment::Center);
        let tools: Element<'_, Message> = container(tools)
            .padding(iced::Padding {
                top: shell::COMPOSER_CONTROL_BOTTOM,
                right: shell::COMPOSER_CONTROL_RIGHT,
                bottom: shell::COMPOSER_CONTROL_BOTTOM,
                left: shell::COMPOSER_CONTROL_LEFT,
            })
            .height(self.reference_tools_height())
            .align_y(iced::Alignment::Center)
            .into();
        let tools = self.font_zoom_region(font_zoom::Group::Chrome, tools);
        let content: Element<'_, Message> = tools;
        container(content)
            .height(self.reference_tools_height())
            .style(move |theme| {
                let mut style = panel(
                    theme,
                    color::COMPOSER,
                    color::LIGHT_BORDER,
                    shell::COMPOSER_RADIUS,
                );
                style.border.width = 0.0;
                if self.config.is_chat_mode() {
                    style.border.radius = iced::border::Radius {
                        top_left: 0.0,
                        top_right: 0.0,
                        bottom_left: shell::COMPOSER_RADIUS,
                        bottom_right: shell::COMPOSER_RADIUS,
                    };
                }
                style
            })
            .into()
    }

    pub(crate) fn reference_context_copy(&self, request: ContextCopy) -> Option<String> {
        let session = self.sessions.get(self.active)?;
        let (session_id, expected, current) = match request {
            ContextCopy::Directory {
                session_id,
                expected,
            } => (session_id, expected, session.cwd_cache.clone()),
            ContextCopy::Command {
                session_id,
                expected,
            } => {
                let running = session.terminal.running_command_text();
                let latest = session
                    .terminal
                    .command_zones
                    .iter()
                    .rev()
                    .find_map(|zone| {
                        zone.command
                            .as_deref()
                            .filter(|command| !command.trim().is_empty())
                    });
                (
                    session_id,
                    expected,
                    context_command(false, running.or(latest)),
                )
            }
        };
        context_copy_value(
            session_id,
            session.id,
            self.reference_private_title(),
            session.managed_remote,
            &expected,
            current,
        )
    }

    fn reference_context_inspector(&self) -> Element<'_, Message> {
        let private = self.reference_private_title();
        let mut content = column![
            self.chrome_section("SESSION CONTEXT"),
            self.chrome_label(
                if private
                    || self
                        .sessions
                        .get(self.active)
                        .is_some_and(|session| session.managed_remote)
                {
                    self.reference_cwd()
                } else {
                    format!("Local directory · {}", self.reference_cwd())
                },
                12.0,
                false,
                false,
            ),
            self.chrome_label(
                format!(
                    "{} terminal session(s) · {} mode",
                    self.sessions.len(),
                    self.config.effective_terminal_mode()
                ),
                10.0,
                true,
                false
            ),
        ]
        .spacing(10);
        if let Some(session) = self.sessions.get(self.active) {
            let (cols, rows) = session.terminal.get_dimensions();
            content = content.push(self.chrome_label(
                format!("Terminal {} · {cols} × {rows} cells", session.id + 1),
                10.0,
                true,
                false,
            ));
            if private {
                content = content.push(self.chrome_label(
                    "Directory, repository and command details are hidden for this private tab.",
                    10.0,
                    true,
                    false,
                ));
            } else if session.managed_remote {
                content = content.push(self.chrome_label(
                    "Remote terminal. A verified remote directory is not available here. Use Files to browse a configured remote host.",
                    10.0,
                    true,
                    false,
                )).push(self.shell_button(
                    "Open Files",
                    Message::ReferenceInspector(InspectorTab::Files),
                    false,
                    false,
                ));
            } else {
                if let Some(git) = session.git_meta_cache.as_ref() {
                    content = content.push(self.chrome_label(
                        jterm_core::git_meta::format_strip(git),
                        10.0,
                        false,
                        false,
                    ));
                }
                let mut directory_actions = row![self.shell_button(
                    "Browse files",
                    Message::ReferenceInspector(InspectorTab::Files),
                    false,
                    false,
                )]
                .spacing(6);
                if let Some(cwd) = session.cwd_cache.as_ref() {
                    directory_actions = directory_actions.push(self.shell_button(
                        "Copy path",
                        Message::ReferenceContextCopy(ContextCopy::Directory {
                            session_id: session.id,
                            expected: cwd.clone(),
                        }),
                        false,
                        false,
                    ));
                }
                content = content.push(directory_actions.wrap());
                if let Some(process) = session.fg_proc_cache.as_ref() {
                    content = content.push(self.chrome_label(
                        format!("Foreground process · {process}"),
                        10.0,
                        false,
                        false,
                    ));
                }
                let running = session.terminal.running_command_text();
                let latest = session.terminal.command_zones.iter().rev().find(|zone| {
                    zone.command
                        .as_deref()
                        .is_some_and(|cmd| !cmd.trim().is_empty())
                });
                let command = context_command(
                    private,
                    running.or_else(|| latest.and_then(|zone| zone.command.as_deref())),
                );
                content = content.push(self.chrome_section(if running.is_some() {
                    "RUNNING COMMAND"
                } else {
                    "LATEST COMMAND"
                }));
                if let Some(command) = command {
                    content = content
                        .push(self.chrome_label(&command, 10.0, false, false))
                        .push(self.shell_button(
                            "Copy shown command",
                            Message::ReferenceContextCopy(ContextCopy::Command {
                                session_id: session.id,
                                expected: command,
                            }),
                            false,
                            false,
                        ));
                } else {
                    content = content.push(self.chrome_label(
                        "No command reported by shell integration yet.",
                        10.0,
                        true,
                        false,
                    ));
                }
                content = content.push(self.chrome_label(
                    context_exit_label(session.last_exit, session.last_duration_ms),
                    10.0,
                    true,
                    false,
                ));
                if let Some(zone) = latest.filter(|zone| !zone.output_preview.is_empty()) {
                    content = content
                        .push(self.chrome_section("LATEST OUTPUT PREVIEW"))
                        .push(self.chrome_label(
                            jterm_core::review_input::safe_multiline_display(
                                &zone.output_preview,
                                4096,
                            ),
                            10.0,
                            true,
                            false,
                        ));
                }
            }
        }
        content = content
            .push(self.shell_button("Open native Vim", Message::OpenVim9Ide, false, false))
            .push(self.chrome_section("AI CONTEXT SETTINGS"))
            .push(self.chrome_label(
                if self.config.ai_enabled {
                    "AI is enabled. These preferences apply when you request AI assistance."
                } else {
                    "AI is disabled. Configure a provider in Settings to use the assistant."
                },
                10.0,
                true,
                false,
            ))
            .push(
                checkbox(self.config.ai_share_command_context)
                    .label("Share command context with cloud AI")
                    .text_size(self.chrome_font_size())
                    .on_toggle(Message::SetAiShareCommandContext),
            )
            .push(
                checkbox(self.config.ai_redact_secrets)
                    .label("Redact secrets in AI-bound text")
                    .text_size(self.chrome_font_size())
                    .on_toggle(Message::SetAiRedactSecrets),
            )
            .push(self.shell_button(
                "AI settings",
                Message::ReferenceCommand(keybindings::Command::ConfigOpen),
                false,
                false,
            ));
        scrollable(container(content).padding(16).width(Length::Fill))
            .height(Length::Fill)
            .into()
    }

    fn reference_agent_inspector(&self) -> Element<'_, Message> {
        let mut content = column![self.chrome_section("AI ASSISTANT")].spacing(10);
        if let Some(session) = self.agent.session.as_ref() {
            content = content
                .push(self.chrome_label(assistant_state_label(session.state()), 12.0, false, false))
                .push(self.chrome_label(
                    format!(
                        "{} / {} turns used",
                        session.turns_used(),
                        session.max_turns()
                    ),
                    10.0,
                    true,
                    false,
                ));
            if !self.agent.provider_label.is_empty() {
                content =
                    content.push(self.chrome_label(&self.agent.provider_label, 10.0, true, false));
            }
            if let Some(bound) = self.agent.bound_session_id {
                content = content.push(self.chrome_label(
                    format!("Bound to terminal {}", bound + 1),
                    10.0,
                    true,
                    false,
                ));
            }
        } else {
            content = content.push(self.chrome_label(
                "Ask the assistant to propose shell commands. Review each command before it runs.",
                10.0,
                true,
                false,
            ));
        }
        if !self.agent.status.is_empty() {
            content = content.push(self.chrome_label(
                jterm_core::review_input::safe_multiline_display(&self.agent.status, 4096),
                10.0,
                true,
                false,
            ));
        }
        let (assistant_label, assistant_action) = if self.agent.is_open {
            ("Close assistant", Message::AgentClose)
        } else if self.config.ai_enabled || self.agent.session.is_some() {
            (
                "Open assistant",
                Message::ReferenceCommand(keybindings::Command::AgentToggle),
            )
        } else {
            (
                "Configure AI assistant",
                Message::ReferenceCommand(keybindings::Command::ConfigOpen),
            )
        };
        content = content
            .push(self.shell_button(assistant_label, assistant_action, false, false))
            .push(self.chrome_section("PROJECT TASKS"));
        if self.config.experimental_task_sidebar {
            let inspector = crate::reference_conversation::observed_inspector(self);
            content = content
                .push(self.chrome_label(&inspector.subtitle, 10.0, true, false))
                .push(self.shell_button("Create task", Message::TaskComposerOpen, false, false))
                .push(self.shell_button(
                    "Open task workspace",
                    Message::TaskPanelToggle,
                    false,
                    false,
                ));
            if inspector.total > 0 {
                content = content
                    .push(self.chrome_label(&inspector.title, 11.0, false, false))
                    .push(self.chrome_label(&inspector.badge, 10.0, true, false));
                for (name, value) in &inspector.metadata {
                    content = content.push(self.chrome_label(
                        format!("{name} · {value}"),
                        9.0,
                        true,
                        false,
                    ));
                }
                if let Some(step) = &inspector.current_step {
                    content = content
                        .push(self.chrome_label(&step.title, 10.0, false, false))
                        .push(self.chrome_label(&step.detail, 10.0, true, false));
                }
            }
        } else {
            content = content
                .push(self.chrome_label(
                    "Enable the Tasks dashboard in Settings to create isolated project tasks.",
                    10.0,
                    true,
                    false,
                ))
                .push(self.shell_button(
                    "Task settings",
                    Message::ReferenceCommand(keybindings::Command::ConfigOpen),
                    false,
                    false,
                ));
        }
        content = content
            .push(self.chrome_section("CODING AGENT TERMINALS"))
            .push(self.chrome_label(
                "Open an installed provider CLI in a new local tab.",
                10.0,
                true,
                false,
            ));
        for provider in agent_task::AgentProvider::ALL {
            content = content.push(self.shell_button(
                format!("Open {}", provider.display_name()),
                Message::AgentLaunch(provider),
                false,
                false,
            ));
        }
        content = content.push(self.shell_button(
            "Saved agent sessions",
            Message::AgentSessionsToggle,
            false,
            false,
        ));
        scrollable(container(content).padding(16).width(Length::Fill))
            .height(Length::Fill)
            .into()
    }

    fn reference_inspector(&self) -> Element<'_, Message> {
        let width = self.reference_inspector_width();
        let tab_width = ((width - 12.0 - 28.0 - 24.0) / 3.0).clamp(1.0, shell::INSPECTOR_TAB_WIDTH);
        let tab = |name: &'static str, value| {
            let content: Element<'_, Message> = if self.chrome_font_size() > 16.0 {
                vector_icon(
                    match value {
                        InspectorTab::Agent => Icon::from_key("agent"),
                        InspectorTab::Context => Icon::PanelRight,
                        InspectorTab::Files => Icon::Files,
                    },
                    rgb(color::CHROME_TEXT),
                    16.0,
                )
            } else {
                self.chrome_label(name, 10.0, false, false).into()
            };
            tooltip(
                button(container(content).center_x(Length::Fill))
                    .on_press(Message::ReferenceInspector(value))
                    .width(tab_width)
                    .height(crate::glass_layout::chrome_control_height(
                        self.chrome_font_size(),
                    ))
                    .padding([4, 5])
                    .style(control(self.reference_inspector_tab == value, false)),
                self.chrome_label(name, 10.0, false, false),
                tooltip::Position::Bottom,
            )
        };
        let header = container(
            row![
                tab("Agent", InspectorTab::Agent),
                tab("Context", InspectorTab::Context),
                tab("Files", InspectorTab::Files),
                Space::new().width(Length::Fill),
                tooltip(
                    button(vector_icon(Icon::Close, rgb(color::CHROME_MUTED), 14.0))
                        .on_press(Message::ReferenceInspectorToggle)
                        .width(28)
                        .height(28)
                        .padding(0)
                        .style(control(false, false)),
                    self.chrome_label("Close inspector", 10.0, false, false),
                    tooltip::Position::Left,
                )
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        )
        .height(72.0_f32.min(self.reference_layout().header.height))
        .padding([2, 6])
        .align_y(iced::Alignment::Center);
        let body: Element<'_, Message> = match self.reference_inspector_tab {
            InspectorTab::Files => mouse_area(
                self.font_zoom_region(font_zoom::Group::Chrome, self.sidebar_files_view()),
            )
            .on_enter(Message::SidebarHover(true))
            .on_exit(Message::SidebarHover(false))
            .into(),
            InspectorTab::Context => self.reference_context_inspector(),
            InspectorTab::Agent
                if self.config.experimental_task_sidebar
                    && self.sidebar_panel == SidebarPanel::Tasks =>
            {
                // Unconsumed arrows/Tab/Enter from task text fields must not
                // reach the native terminal behind the new inspector.
                agent_task_ui::report_input_focus(
                    self.sidebar_tasks_view(),
                    self.task_panel.input_focused,
                    Message::TaskInputFocus,
                )
            }
            InspectorTab::Agent => {
                let fixture = jterm_core::reference_conversation::fixture_enabled();
                if fixture {
                    crate::reference_conversation::inspector_view(
                        &jterm_core::reference_conversation::acceptance_fixture().inspector,
                        true,
                        self.chrome_font_size(),
                        self.reference_layout().center.height
                            - 72.0_f32.min(self.reference_layout().header.height),
                    )
                } else {
                    self.reference_agent_inspector()
                }
            }
        };
        container(column![header, body].height(Length::Fill))
            .width(width)
            .height(Length::Fill)
            .style(|theme| {
                panel(
                    theme,
                    color::INSPECTOR,
                    color::DARK_BORDER,
                    shell::CARD_RADIUS,
                )
            })
            .into()
    }

    pub(crate) fn reference_input_metrics(
        &self,
    ) -> Option<crate::native_input_surface::DockMetrics> {
        if !self.config.is_chat_mode() || self.is_split() {
            return None;
        }
        let session = self.sessions.get(self.active)?;
        if session.transcript_read_only() || self.vim9_tabs.contains_key(&session.id) {
            return None;
        }
        let dock = session
            .projection
            .chat_rows()
            .iter()
            .position(|row| row.role == crate::chat_mode::ChatRole::Composer)?;
        let last_raw = session
            .projection
            .chat_rows()
            .iter()
            .enumerate()
            .rev()
            .find(|(row, kind)| {
                kind.role == crate::chat_mode::ChatRole::Composer
                    && matches!(
                        session.projection.row_kinds().get(*row),
                        Some(terminal::ProjectedRowKind::Raw)
                    )
            })
            .map(|(row, _)| row)
            .unwrap_or(dock);
        let rows = last_raw.max(session.cursor.0).saturating_sub(dock) + 1;
        let cap = self.reference_layout().input_cap(
            self.block_reading_bar_height(),
            self.startup_recovery.banner_height(self.chrome_font_size()),
        );
        Some(crate::native_input_surface::dock_metrics(
            rows,
            self.metrics.cell_h,
            cap,
        ))
    }

    pub(crate) fn reference_native_budget(&self) -> crate::glass_layout::NativeBudget {
        crate::native_viewport::native_budget(
            self.reference_layout(),
            self.block_reading_bar_height(),
            self.startup_recovery.banner_height(self.chrome_font_size()),
            self.reference_input_metrics()
                .map(|metrics| metrics.input_height),
        )
    }

    pub(crate) fn reference_terminal_surface<'a>(
        &'a self,
        terminal: Element<'a, Message>,
    ) -> Element<'a, Message> {
        if !self.config.is_chat_mode() {
            return terminal;
        }
        macro_rules! direct {
            () => {
                crate::native_input_surface::surface(
                    terminal,
                    None,
                    None,
                    "",
                    self.reference_native_budget().raw.height,
                )
            };
        }
        let Some(session) = self.sessions.get(self.active) else {
            return direct!();
        };
        if self.is_split() {
            return direct!();
        }
        if self.vim9_tabs.contains_key(&session.id) {
            return direct!();
        }
        let Some(dock) = session
            .projection
            .chat_rows()
            .iter()
            .position(|row| row.role == crate::chat_mode::ChatRole::Composer)
        else {
            return direct!();
        };
        let margin = if session.transcript_canvas.enabled() && session.projection.has_canvas_head()
        {
            self.metrics.cell_h
        } else {
            0.0
        };
        let last_raw = session
            .projection
            .chat_rows()
            .iter()
            .enumerate()
            .rev()
            .find(|(row, kind)| {
                kind.role == crate::chat_mode::ChatRole::Composer
                    && matches!(
                        session.projection.row_kinds().get(*row),
                        Some(terminal::ProjectedRowKind::Raw)
                    )
            })
            .map(|(row, _)| row)
            .unwrap_or(dock);
        let needed_rows = last_raw.max(session.cursor.0).saturating_sub(dock) + 1;
        let Some(dock_metrics) = self.reference_input_metrics() else {
            return direct!();
        };
        let budget = self.reference_native_budget();
        debug_assert_eq!(dock_metrics.input_height, budget.input_height);
        log::debug!(target: "frost::reference_tools", "native_dock rows={} cell_h={} padding={} height={} visible_rows={}",
            needed_rows, self.metrics.cell_h, dock_metrics.vertical_padding,
            dock_metrics.input_height, dock_metrics.visible_rows);
        // Very long drafts scroll inside this bounded native input surface.
        // Pick an origin that includes the actual caret, rather than clipping
        // the first two rows while an IME edits below them.
        let visible_start = dock.max(
            session
                .cursor
                .0
                .saturating_add(1)
                .saturating_sub(dock_metrics.visible_rows),
        );
        let dock_y = self.metrics.padding + visible_start as f32 * self.metrics.cell_h - margin;
        let history = jterm_core::reference_conversation::fixture_enabled().then(|| {
            crate::reference_conversation::conversation_view(
                &jterm_core::reference_conversation::acceptance_fixture(),
                self.effective_font_size(),
            )
        });
        crate::native_input_surface::surface(
            terminal,
            history,
            Some(crate::native_input_surface::Dock {
                y: dock_y,
                history_end_y: self.metrics.padding + dock as f32 * self.metrics.cell_h - margin,
                input_height: dock_metrics.input_height,
                vertical_padding: dock_metrics.vertical_padding,
            }),
            "",
            budget.raw.height,
        )
    }

    pub(crate) fn trace_reference_tools(&self, event: &str) {
        if let Some(session) = self.sessions.get(self.active) {
            log::debug!(target: "frost::reference_tools", "{event} open={} raw={:?} position_rows={} following={} projection={:?}",
                self.reference_tools_selection.is_some(), self.reference_native_budget().raw,
                session.transcript_canvas.position_rows(), session.transcript_canvas.following(), session.projection.key());
        }
    }

    /// Opt-in transition metadata only: no command, task, file or output text.
    pub(crate) fn trace_reference_inspector(&self, event: &str) {
        if !log::log_enabled!(target: "frost::reference_inspector", log::Level::Debug) {
            return;
        }
        if let Some(session) = self.sessions.get(self.active) {
            let anchor = (0..session.projection.cells().len()).find_map(|row| {
                session
                    .projection
                    .view_to_raw(terminal::ViewportCell { row, col: 0 })
            });
            log::debug!(target: "frost::reference_inspector",
                "{event} open={} drawer={} visible={} tab={:?} center={:?} session={} bash_pid={} tabs={} active_tab={} position_rows={} following={} anchor={:?} input_active={}",
                self.reference_inspector_open, self.reference_inspector_drawer,
                self.reference_inspector_visible(), self.reference_inspector_tab,
                self.reference_layout().center, session.id, session.pty.get_child_pid(),
                self.tabs.len(), self.active_tab, session.transcript_canvas.position_rows(),
                session.transcript_canvas.following(), anchor, self.terminal_input_active());
        }
    }

    fn reference_tools_popover(&self) -> Element<'_, Message> {
        use jterm_core::reference_popup as popup;
        let border = self.c_border();
        let muted = self.c_text_dim();
        let mut rows = column![];
        for (index, item) in popup::COMMANDS.iter().enumerate() {
            if item.separator_before {
                rows = rows.push(container(Space::new().height(1)).width(Length::Fill).style(
                    move |_| container::Style {
                        background: Some(border.into()),
                        ..Default::default()
                    },
                ));
            }
            let selected = self.reference_tools_selection == Some(index);
            let fg = if item.placeholder {
                muted
            } else {
                self.c_text()
            };
            let row = button(
                row![
                    vector_icon(Icon::from_key(item.icon_key), fg, popup::ICON_SIZE),
                    self.chrome_label(item.title, 10.0, false, false).color(fg),
                    Space::new().width(Length::Fill),
                    self.chrome_label(item.description, 9.0, true, false)
                        .color(muted)
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            )
            .height(popup::ROW_HEIGHT)
            .width(Length::Fill)
            .padding([5, 7])
            .on_press_maybe((!item.placeholder).then_some(Message::ReferenceToolsChoose(index)))
            .style(move |_, status| theme::popup_button_style(&self.theme, selected, status));
            rows = rows.push(mouse_area(row).on_enter(Message::ReferenceToolsSelect(index)));
        }
        let menu = container(rows)
            .width(popup::WIDTH)
            .padding(popup::PADDING)
            .style(self.popup_style());
        let layout = self.reference_layout();
        let left = (layout.center.x + shell::COMPOSER_INSET + 270.0)
            .min((self.win_size.width - popup::WIDTH - 8.0).max(0.0));
        let height = popup::COMMANDS.len() as f32 * popup::ROW_HEIGHT + 2.0 * popup::PADDING + 1.0;
        let top = (layout.composer.y + shell::COMPOSER_CONTROL_BOTTOM - 8.0 - height)
            .max(shell::TITLEBAR_HEIGHT);
        container(opaque(menu))
            .padding(iced::Padding {
                left,
                top,
                ..iced::Padding::ZERO
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    pub(crate) fn reference_workspace<'a>(
        &'a self,
        terminal: Element<'a, Message>,
    ) -> Element<'a, Message> {
        use crate::glass_shell_tokens as glass;
        let layout = self.reference_layout();
        let budget = self.reference_native_budget();
        let main_bottom = (self.win_size.height - layout.center.bottom()).max(0.0);
        let navigation_bottom = (self.win_size.height - layout.rail.bottom()).max(0.0);
        let terminal = container(terminal)
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true)
            .style(move |theme| {
                if budget.editor.is_some() {
                    container::Style::default()
                } else {
                    container::Style {
                        background: Some(
                            shell::rgba(color::TERMINAL, theme.palette().background.a).into(),
                        ),
                        ..Default::default()
                    }
                }
            });
        let pane = container(terminal)
            .padding(iced::Padding {
                top: glass::TERMINAL_TOP_GAP,
                right: glass::TERMINAL_INSET_RIGHT,
                bottom: if budget.editor.is_some() {
                    0.0
                } else {
                    glass::TERMINAL_FOOTER_GAP
                },
                left: glass::TERMINAL_INSET_LEFT,
            })
            .height(Length::Fill);
        let tools = container(self.reference_tools())
            .padding(iced::Padding {
                left: glass::TOOLBAR_INSET_LEFT,
                right: glass::TOOLBAR_INSET_RIGHT,
                ..iced::Padding::ZERO
            })
            .height(layout.composer.height);
        let center = container(
            column![
                self.font_zoom_region(font_zoom::Group::Chrome, self.reference_header()),
                pane,
                tools
            ]
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|theme| {
            panel(
                theme,
                color::CANVAS,
                color::DARK_BORDER,
                glass::PANEL_RADIUS,
            )
        });
        let mut body = row![column![
            self.font_zoom_region(font_zoom::Group::Chrome, self.reference_rail()),
            Space::new().height(navigation_bottom)
        ]
        .width(layout.rail.width)
        .height(Length::Fill)]
        .height(Length::Fill);
        if let Some(nav) = layout.navigation {
            body = body.push(
                column![
                    self.font_zoom_region(font_zoom::Group::Chrome, self.reference_navigation()),
                    Space::new().height(navigation_bottom)
                ]
                .width(nav.width)
                .height(Length::Fill),
            );
        }
        body = body.push(if layout.navigation.is_some() {
            column![
                Space::new().height(glass::MAIN_TOP - glass::TOPBAR_HEIGHT),
                self.reference_resize_grip(
                    crate::sidebar_resize::Side::Navigation,
                    glass::LEFT_GAP
                ),
                Space::new().height(main_bottom)
            ]
            .width(glass::LEFT_GAP)
            .height(Length::Fill)
            .into()
        } else {
            Element::from(Space::new().width(glass::LEFT_GAP))
        });
        body = body.push(
            column![
                Space::new().height(glass::MAIN_TOP - glass::TOPBAR_HEIGHT),
                center,
                Space::new().height(main_bottom)
            ]
            .width(Length::Fill)
            .height(Length::Fill),
        );
        if let Some(inspector) = layout.inspector {
            body = body.push(
                column![
                    Space::new().height(glass::MAIN_TOP - glass::TOPBAR_HEIGHT),
                    self.reference_resize_grip(
                        crate::sidebar_resize::Side::Inspector,
                        glass::INSPECTOR_GAP
                    ),
                    Space::new().height(main_bottom)
                ]
                .width(glass::INSPECTOR_GAP)
                .height(Length::Fill),
            );
            body = body.push(
                column![
                    Space::new().height(glass::MAIN_TOP - glass::TOPBAR_HEIGHT),
                    self.font_zoom_region(font_zoom::Group::Chrome, self.reference_inspector()),
                    Space::new().height(main_bottom)
                ]
                .width(inspector.width)
                .height(Length::Fill),
            );
        }
        let body = container(body)
            .padding(iced::Padding {
                left: glass::OUTER_LEFT,
                right: glass::OUTER_RIGHT,
                ..iced::Padding::ZERO
            })
            .height(Length::Fill);
        let mut workspace =
            column![self.font_zoom_region(font_zoom::Group::Chrome, self.reference_topbar())];
        if let Some(banner) = self.session_recovery_banner() {
            workspace = workspace.push(banner);
        }
        let workspace: Element<'_, Message> = workspace
            .push(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let (dismiss, inspector): (Element<'_, Message>, Element<'_, Message>) =
            if layout.inspector.is_none() && self.reference_inspector_drawer {
                let dismiss = container(
                    button(Space::new().width(Length::Fill).height(Length::Fill))
                        .on_press(Message::ReferenceInspectorToggle)
                        .padding(0)
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .style(|_, _| button::Style {
                            background: Some(Color::from_rgba(0.02, 0.04, 0.09, 0.18).into()),
                            ..Default::default()
                        }),
                )
                .padding(iced::Padding {
                    top: layout.titlebar.height,
                    ..iced::Padding::ZERO
                })
                .width(Length::Fill)
                .height(Length::Fill);
                let inspector = container(
                    row![
                        self.reference_resize_grip(
                            crate::sidebar_resize::Side::Inspector,
                            glass::INSPECTOR_GAP
                        ),
                        self.font_zoom_region(font_zoom::Group::Chrome, self.reference_inspector()),
                    ]
                    .width(self.reference_inspector_width() + glass::INSPECTOR_GAP),
                )
                .padding(iced::Padding {
                    top: layout.center.y,
                    right: glass::OUTER_RIGHT,
                    bottom: main_bottom,
                    ..iced::Padding::ZERO
                })
                .align_right(Length::Fill)
                .height(Length::Fill);
                (dismiss.into(), inspector.into())
            } else {
                (
                    Space::new().width(0).height(0).into(),
                    Space::new().width(0).height(0).into(),
                )
            };
        // Constant base index preserves the native terminal/model through toggles.
        let workspace = crate::native_popup_surface::layers(workspace, dismiss, inspector);
        let (dismiss, menu): (Element<'_, Message>, Element<'_, Message>) =
            if self.reference_tools_selection.is_some() {
                let dismiss = button(Space::new().width(Length::Fill).height(Length::Fill))
                    .on_press(Message::ReferenceToolsClose)
                    .padding(0)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(|_, _| button::Style::default())
                    .into();
                (dismiss, self.reference_tools_popover())
            } else {
                (
                    Space::new().width(0).height(0).into(),
                    Space::new().width(0).height(0).into(),
                )
            };
        crate::native_font_probe::observe(crate::native_popup_surface::layers(
            workspace, dismiss, menu,
        ))
    }
}

#[cfg(test)]
mod panel_tests {
    use super::*;

    #[test]
    fn workspace_picker_keeps_all_tabs_scrollable_within_the_window() {
        assert_eq!(
            tab_switcher_panel_size(Size::new(1280.0, 900.0)),
            Size::new(420.0, 420.0)
        );
        assert_eq!(
            tab_switcher_panel_size(Size::new(300.0, 240.0)),
            Size::new(268.0, 208.0)
        );
        assert_eq!(
            tab_switcher_panel_size(Size::new(0.0, 0.0)),
            Size::new(1.0, 1.0)
        );
    }

    #[test]
    fn private_context_never_exposes_a_command_or_a_copy_payload() {
        let command = "deploy --token=private /sensitive/workspace";
        assert_eq!(context_command(true, Some(command)), None);
        assert_eq!(context_command(false, None), None);
        assert_eq!(context_command(false, Some("  ")), None);
        assert_eq!(
            context_command(false, Some("cargo test")),
            Some("cargo test".into())
        );
    }

    #[test]
    fn context_command_bounds_unicode_and_makes_controls_visible() {
        let rendered = context_command(false, Some("echo safe\u{202e}\n\u{1b}hidden")).unwrap();
        assert!(!rendered.contains('\u{202e}'));
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.contains('\u{fffd}'));
        let rendered = context_command(false, Some(&"界".repeat(4096))).unwrap();
        assert!(rendered.len() <= 4096);
        assert!(rendered.ends_with('…'));
    }

    #[test]
    fn context_copy_rejects_stale_private_remote_and_changed_values() {
        let current = Some("/workspace/project".to_owned());
        assert_eq!(
            context_copy_value(2, 2, false, false, "/workspace/project", current.clone()),
            current
        );
        assert_eq!(
            context_copy_value(2, 3, false, false, "/workspace/project", current.clone()),
            None
        );
        assert_eq!(
            context_copy_value(2, 2, true, false, "/workspace/project", current.clone()),
            None
        );
        assert_eq!(
            context_copy_value(2, 2, false, true, "/workspace/project", current.clone()),
            None
        );
        assert_eq!(
            context_copy_value(2, 2, false, false, "/old/project", current),
            None
        );
        assert_eq!(
            context_copy_value(2, 2, false, false, "/workspace/project", None),
            None
        );
    }

    #[test]
    fn context_distinguishes_unknown_exit_from_success_and_failure() {
        assert_eq!(context_exit_label(None, None), "No recorded command exit");
        assert_eq!(
            context_exit_label(Some(0), Some(1250)),
            "Last command succeeded · exit 0 · 1.25s"
        );
        assert_eq!(
            context_exit_label(Some(7), None),
            "Last command exited with 7"
        );
    }

    #[test]
    fn assistant_activity_tracks_real_approval_and_completion_states() {
        let mut session = jterm_core::agent::AgentSession::new(4);
        assert_eq!(assistant_state_label(session.state()), "Ready for a task");
        session.submit_user("inspect the directory").unwrap();
        assert_eq!(
            assistant_state_label(session.state()),
            "Waiting for the model"
        );
        session
            .accept_model_reply(r#"{"action":"run","command":"pwd"}"#)
            .unwrap();
        assert_eq!(
            assistant_state_label(session.state()),
            "Command awaiting your approval"
        );
        session.cancel();
        assert_eq!(assistant_state_label(session.state()), "Session cancelled");

        let mut completed = jterm_core::agent::AgentSession::new(4);
        completed.submit_user("finish").unwrap();
        completed
            .accept_model_reply(r#"{"action":"done","message":"complete"}"#)
            .unwrap();
        assert_eq!(assistant_state_label(completed.state()), "Task completed");
    }
}
