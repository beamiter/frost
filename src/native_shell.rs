//! Native Iced binding for the family's measured workspace contract.
//! The terminal widget remains the sole PTY, selection and IME input owner.

use super::*;
use crate::glass_profile::{self as shell, color};
use crate::reference_icon::{icon as vector_icon, Icon};
use iced::widget::{column, opaque};

pub(crate) const CONTENT_TOP: f32 = crate::native_viewport::CONTENT_TOP;
pub(crate) const TOOLS_HEIGHT: f32 = crate::native_viewport::TOOLS_HEIGHT;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum InspectorTab {
    #[default]
    Agent,
    Context,
    Files,
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
        shadow: if radius >= crate::glass_shell_tokens::PANEL_RADIUS {
            iced::Shadow {
                color: Color::from_rgba8(71, 115, 255, 0.16),
                offset: iced::Vector::new(0.0, 0.0),
                blur_radius: 18.0,
            }
        } else {
            iced::Shadow::default()
        },
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
                color: if light {
                    rgb(color::CONTROL_BORDER)
                } else {
                    Color::TRANSPARENT
                },
                width: if light { 1.0 } else { 0.0 },
                radius: shell::CONTROL_RADIUS.into(),
                ..Default::default()
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
        let mut layout = shell::Layout::for_size(self.win_size.width, self.win_size.height);
        if !self.sidebar_open {
            layout = layout.without_navigation();
        }
        if !self.reference_inspector_open {
            layout = layout.without_inspector();
        }
        layout
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
            .and_then(Session::cwd_display)
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
            .height(shell::CONTROL_HEIGHT)
            .padding([4, 8])
            .style(control(selected, light))
    }

    fn vector_button(
        &self,
        glyph: Icon,
        action: Message,
        selected: bool,
    ) -> iced::widget::Button<'static, Message> {
        button(container(vector_icon(glyph, rgb(color::CHROME_TEXT), 20.0)).center(44))
            .on_press(action)
            .width(44)
            .height(44)
            .padding(0)
            .style(control(selected, true))
    }

    pub(crate) fn reference_topbar(&self) -> Element<'_, Message> {
        let window_control = |glyph: &'static str, action: Message| {
            button(container(self.chrome_label(glyph, 12.0, false, false)).center(24))
                .on_press(action)
                .width(24)
                .height(24)
                .padding(0)
                .style(control(false, false))
        };
        let controls = row![
            window_control("×", Message::WindowClose),
            window_control("−", Message::WindowMinimize),
            window_control("□", Message::WindowToggleMaximize),
        ]
        .spacing(0)
        .width(105.0)
        .align_y(iced::Alignment::Center);
        let identity = row![
            crate::reference_icon::identity("frost", 32.0),
            self.chrome_label(
                if self.win_size.width >= 560.0 {
                    "Frost"
                } else {
                    ""
                },
                16.0,
                false,
                false
            ),
        ]
        .spacing(14)
        .align_y(iced::Alignment::Center);
        let path = self.reference_cwd();
        let path: Element<'_, Message> = if self.win_size.width >= 1050.0 {
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
        let search: Element<'_, Message> = if self.win_size.width >= 850.0 {
            button(
                row![
                    vector_icon(Icon::Search, rgb(color::CHROME_MUTED), 20.0),
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
                400.0
            } else {
                230.0
            }))
            .height(shell::SEARCH_HEIGHT)
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
            self.shell_button("⌕", Message::ReferencePalette, false, false)
                .into()
        };
        let mode = self.config.effective_terminal_mode();
        let search_width = if self.win_size.width >= 1500.0 {
            shell::SEARCH_WIDTH
        } else if self.win_size.width >= 1250.0 {
            400.0
        } else if self.win_size.width >= 850.0 {
            230.0
        } else {
            28.0
        };
        let search_x = (self.win_size.width - search_width) * 0.5 + shell::SEARCH_CENTER_OFFSET;
        let mode_width = shell::MODE_CONTROL_MIN_WIDTH * (self.chrome_font_size() / 14.0).max(1.0);
        let right_minimum = mode_width * 2.0
            + 28.0
            + if self.win_size.width >= 500.0 {
                28.0 + 4.0 * shell::METADATA_GAP
            } else {
                3.0 * shell::METADATA_GAP
            };
        let left_width = (search_x - 14.0)
            .max(106.0)
            .min((self.win_size.width - search_width - right_minimum - 28.0).max(106.0));
        let left = row![controls, identity, path]
            .spacing(14)
            .align_y(iced::Alignment::Center)
            .width(left_width);
        let mode_button = |caption, mode, selected| {
            button(container(self.chrome_label(caption, 11.0, false, false)).center_x(Length::Fill))
                .on_press(Message::SetTerminalMode(mode))
                .width(mode_width)
                .height(shell::MODE_CONTROL_HEIGHT)
                .padding([4, 8])
                .style(mode_control(selected))
        };
        let mut right = row![
            Space::new().width(Length::Fill),
            mode_button(
                "Block",
                config::TerminalMode::Block,
                mode != config::TerminalMode::Chat,
            ),
            mode_button(
                "Chat",
                config::TerminalMode::Chat,
                mode == config::TerminalMode::Chat,
            ),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center)
        .width(Length::Fill);
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
        .height(shell::TITLEBAR_HEIGHT)
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
                button(container(vector_icon(glyph, rgb(color::CHROME_TEXT), 24.0)).center(50))
                    .on_press(action)
                    .width(50)
                    .height(50)
                    .padding(0)
                    .style(control(selected, false)),
                self.chrome_label(hint, 11.0, false, false),
                tooltip::Position::Right,
            )
            .into()
        };
        let mut rail = column![].spacing(12).align_x(iced::Alignment::Center);
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
                    container(self.chrome_label(initial, 12.0, false, false).center())
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
            top: 12.0,
            right: 0.0,
            bottom: 10.0,
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
            .filter(|_| !self.reference_private_title())
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
            container(
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
            .height(shell::WORKSPACE_SELECTOR_HEIGHT)
            .padding([6, 9])
            .style(|theme| panel(theme, color::INSPECTOR_CARD, color::DARK_BORDER, 8.0)),
            self.chrome_label(format!("{cwd} · {branch_metadata}"), 10.0, false, false),
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
            destinations = destinations.push(
                button(
                    row![
                        vector_icon(Icon::from_key(key), rgb(color::CHROME_MUTED), 15.0),
                        self.chrome_label(title, 10.0, false, false)
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
                )
                .on_press(action)
                .height(shell::NAVIGATION_ROW_HEIGHT)
                .width(Length::Fill)
                .padding([5, 8])
                .style(control(index == 0, false)),
            );
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
                    .height(self.chrome_row_height(18.0, shell::RECENT_ROW_HEIGHT, 7.0))
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
                        self.chrome_label(short(&self.tab_label(index), 22), 10.0, false, false)
                            .width(Length::Fill)
                    ]
                    .spacing(7),
                )
                .height(self.chrome_row_height(18.0, shell::RECENT_ROW_HEIGHT, 7.0))
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
            spaces = spaces.push(
                container(
                    row![
                        crate::reference_icon::identity(name, 32.0),
                        column![
                            self.chrome_label(*name, 10.0, false, false),
                            self.chrome_label(
                                if fixture || selected {
                                    *description
                                } else {
                                    "Unavailable here"
                                },
                                8.0,
                                true,
                                false
                            )
                        ]
                        .spacing(3)
                    ]
                    .spacing(9)
                    .align_y(iced::Alignment::Center),
                )
                .height(self.chrome_row_height(18.0, shell::SPACES_ROW_HEIGHT, 7.0))
                .padding([2, 6]),
            );
        }
        let navigation = column![
            Space::new().height(shell::NAVIGATION_LABEL_TOP - shell::TITLEBAR_HEIGHT),
            container(self.chrome_label("WORKSPACE", 9.0, true, false))
                .height(shell::WORKSPACE_SELECTOR_TOP - shell::NAVIGATION_LABEL_TOP),
            selector,
            Space::new().height(
                shell::NAVIGATION_ITEMS_TOP
                    - shell::WORKSPACE_SELECTOR_TOP
                    - shell::WORKSPACE_SELECTOR_HEIGHT
            ),
            destinations,
            Space::new().height(8),
            container(self.chrome_section("TABS")).height(20),
            container(recent).height(
                177.0_f32
                    .max(5.0 * self.chrome_row_height(18.0, shell::RECENT_ROW_HEIGHT, 7.0) + 12.0)
            ),
            Space::new().height(7),
            container(self.chrome_section("SPACES")).height(20),
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
        container(column![
            scrollable(navigation).height(Length::Fill),
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
                        self.chrome_label("Local session", 10.0, false, false)
                    ]
                    .spacing(7)
                    .align_y(iced::Alignment::Center),
                    self.chrome_label(status, 8.0, true, false)
                ]
                .spacing(0)
            )
            .height(shell::NAVIGATION_STATUS_HEIGHT)
            .width(Length::Fill)
        ])
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
        let mode = if self.config.is_chat_mode() {
            "Chat"
        } else {
            "Block"
        };
        let name = if jterm_core::reference_conversation::fixture_enabled() {
            jterm_core::reference_conversation::acceptance_fixture().title
        } else {
            self.tab_label(self.active_tab)
        };
        let heading = column![
            self.chrome_label(
                if name.starts_with("Workspace / ") {
                    name.clone()
                } else {
                    format!("Workspace / {}", short(&name, 55))
                },
                14.667,
                false,
                true
            )
            .font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..iced::Font::with_name(shell::FONT_SANS)
            }),
            self.chrome_label(
                if jterm_core::reference_conversation::fixture_enabled() {
                    jterm_core::reference_conversation::FIXTURE_LABEL.to_string()
                } else {
                    format!("{mode}  ·  Local session")
                },
                10.0,
                true,
                true
            )
        ]
        .spacing(3);
        let title = row![
            container(crate::reference_icon::identity("frost", 40.0))
                .center(54.0)
                .style(|theme| panel(
                    theme,
                    color::CONTROL_SURFACE,
                    color::LIGHT_BORDER,
                    shell::CONTROL_RADIUS
                )),
            heading
        ]
        .spacing(16)
        .align_y(iced::Alignment::Center);
        container(
            row![
                title,
                Space::new().width(Length::Fill),
                button(
                    container(vector_icon(Icon::Plus, rgb(color::CANVAS_MUTED), 16.0))
                        .center(shell::HEADER_ACTION_SIZE)
                )
                .on_press(Message::NewSession)
                .width(shell::HEADER_ACTION_SIZE)
                .height(shell::HEADER_ACTION_SIZE)
                .padding(0)
                .style(control(false, true)),
                button(
                    container(vector_icon(Icon::Search, rgb(color::CANVAS_MUTED), 16.0))
                        .center(shell::HEADER_ACTION_SIZE)
                )
                .on_press(Message::ReferenceCommand(keybindings::Command::SearchOpen))
                .width(shell::HEADER_ACTION_SIZE)
                .height(shell::HEADER_ACTION_SIZE)
                .padding(0)
                .style(control(false, true)),
                button(
                    container(vector_icon(Icon::More, rgb(color::CANVAS_MUTED), 16.0))
                        .center(shell::HEADER_ACTION_SIZE)
                )
                .on_press(Message::ReferencePalette)
                .width(shell::HEADER_ACTION_SIZE)
                .height(shell::HEADER_ACTION_SIZE)
                .padding(0)
                .style(control(false, true))
            ]
            .spacing(shell::HEADER_ACTION_GAP)
            .align_y(iced::Alignment::Center),
        )
        .height(self.reference_layout().header.height)
        .padding([6.0, shell::COMPOSER_INSET])
        .align_y(iced::Alignment::Center)
        .style(|theme| {
            let mut style = panel(theme, color::CANVAS, color::LIGHT_BORDER, 0.0);
            style.text_color = Some(rgb(color::CANVAS_TEXT));
            style
        })
        .into()
    }

    pub(crate) fn reference_tools_height(&self) -> f32 {
        TOOLS_HEIGHT
    }

    fn reference_tools(&self) -> Element<'_, Message> {
        let font_scale = (self.chrome_font_size() / 14.0).max(1.0);
        // Use the rendered composer's inner surface, including sidebar toggles,
        // rather than the original layout's cached composer rectangle.
        let composer_width = self.reference_layout().composer.width;
        let available = composer_width / font_scale;
        let tool = |glyph: Icon,
                    title: &'static str,
                    width: f32,
                    action: Option<Message>|
         -> Element<'_, Message> {
            let enabled = action.is_some();
            let fg = if enabled {
                color::CANVAS_MUTED
            } else {
                color::DISABLED_TEXT
            };
            let is_model = title == shell::MODEL_UNAVAILABLE_LABEL;
            let mut contents = row![]
                .spacing(jterm_core::reference_popup::FOOTER_ICON_LABEL_GAP)
                .align_y(iced::Alignment::Center);
            if !is_model {
                contents = contents.push(vector_icon(glyph, rgb(fg), 18.0));
            }
            let caption = self
                .chrome_label(
                    title,
                    jterm_core::reference_popup::FOOTER_LABEL_SIZE,
                    !enabled,
                    true,
                )
                .color(rgb(fg))
                .wrapping(text::Wrapping::None);
            if is_model {
                contents = contents
                    .push(container(caption).width(Length::Fill).clip(true))
                    .push(vector_icon(Icon::ChevronDown, rgb(fg), 18.0))
                    .spacing(4);
            } else {
                contents = contents.push(caption);
                if title == jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[4] {
                    contents = contents.push(vector_icon(Icon::ChevronDown, rgb(fg), 18.0));
                }
            }
            let button = button(contents)
                .on_press_maybe(action)
                .height(shell::CONTROL_HEIGHT)
                .width(width * font_scale)
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
                        width: 1.0,
                        radius: shell::CONTROL_RADIUS.into(),
                    };
                    style
                });
            if enabled {
                button.into()
            } else {
                tooltip(
                    button,
                    self.chrome_label(format!("{title} · Not available yet"), 10.0, true, true),
                    tooltip::Position::Top,
                )
                .into()
            }
        };
        let mut tools = row![button(
            container(vector_icon(Icon::Plus, rgb(color::CANVAS_TEXT), 18.0))
                .center(shell::COMPOSER_PLUS_WIDTH)
        )
        .on_press(Message::NewSession)
        .width(shell::COMPOSER_PLUS_WIDTH)
        .height(shell::CONTROL_HEIGHT)
        .padding(0)
        .style(control(false, true))]
        .spacing(shell::METADATA_GAP)
        .align_y(iced::Alignment::Center);
        if available >= shell::COMPOSER_SECONDARY_MIN_WIDTH {
            tools = tools
                .push(tool(
                    Icon::Attach,
                    jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[0],
                    shell::COMPOSER_ATTACH_WIDTH,
                    None,
                ))
                .push(tool(
                    Icon::Image,
                    jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[1],
                    shell::COMPOSER_IMAGE_WIDTH,
                    None,
                ))
                .push(tool(
                    Icon::Code,
                    jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[2],
                    shell::COMPOSER_CODE_WIDTH,
                    None,
                ));
        }
        if available >= shell::COMPOSER_TERMINAL_MIN_AVAILABLE {
            tools = tools.push(tool(
                Icon::Terminal,
                jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[3],
                shell::COMPOSER_TERMINAL_WIDTH,
                Some(Message::ReferenceFocusTerminal),
            ));
        }
        tools = tools.push(tool(
            Icon::Grid,
            jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[4],
            shell::COMPOSER_TOOLS_WIDTH,
            Some(Message::ReferenceToolsToggle),
        ));
        let mut controls = row![]
            .spacing(shell::METADATA_GAP)
            .align_y(iced::Alignment::Center);
        if available >= shell::COMPOSER_MODEL_MIN_AVAILABLE {
            controls = controls.push(tool(
                Icon::ChevronDown,
                shell::MODEL_UNAVAILABLE_LABEL,
                shell::COMPOSER_MODEL_MIN_WIDTH,
                None,
            ));
        }
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
        .height(shell::CONTROL_HEIGHT)
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
            .height(TOOLS_HEIGHT)
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
                style.border.color = composer_border_color();
                if self.config.is_chat_mode() {
                    style.border.radius = iced::border::Radius {
                        top_left: 0.0,
                        top_right: 0.0,
                        bottom_left: 14.0,
                        bottom_right: 14.0,
                    };
                }
                style
            })
            .into()
    }

    fn reference_inspector(&self) -> Element<'_, Message> {
        let tab = |name: &'static str, value| {
            button(self.chrome_label(name, 10.0, false, false))
                .on_press(Message::ReferenceInspector(value))
                .width(shell::INSPECTOR_TAB_WIDTH)
                .height(44.0)
                .padding([4, 5])
                .style(control(self.reference_inspector_tab == value, false))
        };
        let header = container(
            row![
                tab("Agent", InspectorTab::Agent),
                tab("Context", InspectorTab::Context),
                tab("Files", InspectorTab::Files),
                Space::new().width(Length::Fill),
                self.chrome_label("⊞", 13.0, true, false)
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        )
        .height(72.0_f32.min(self.reference_layout().header.height))
        .padding([5, 12])
        .align_y(iced::Alignment::Center);
        let body: Element<'_, Message> = match self.reference_inspector_tab {
            InspectorTab::Files => mouse_area(
                self.font_zoom_region(font_zoom::Group::Chrome, self.sidebar_files_view()),
            )
            .on_enter(Message::SidebarHover(true))
            .on_exit(Message::SidebarHover(false))
            .into(),
            InspectorTab::Context => {
                let session = self.sessions.get(self.active);
                let cwd = self.reference_cwd();
                let git = session
                    .filter(|_| !self.reference_private_title())
                    .and_then(|s| s.git_meta_cache.as_ref())
                    .map(jterm_core::git_meta::format_strip)
                    .unwrap_or_else(|| "No repository context".into());
                container(
                    column![
                        self.chrome_label("SESSION CONTEXT", 9.0, true, false),
                        self.chrome_label(cwd, 12.0, false, false),
                        self.chrome_label(git, 10.0, true, false),
                        self.chrome_label(
                            format!("{} terminal session(s)", self.sessions.len()),
                            10.0,
                            true,
                            false
                        ),
                        self.shell_button("Open native Vim", Message::OpenVim9Ide, false, false),
                        self.shell_button(
                            "Agent sessions",
                            Message::AgentSessionsToggle,
                            false,
                            false
                        )
                    ]
                    .spacing(12),
                )
                .padding(16)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
            }
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
                let inspector = if fixture {
                    jterm_core::reference_conversation::acceptance_fixture().inspector
                } else {
                    crate::reference_conversation::observed_inspector(self)
                };
                crate::reference_conversation::inspector_view(
                    &inspector,
                    fixture,
                    self.chrome_font_size(),
                    self.reference_layout().center.height
                        - 72.0_f32.min(self.reference_layout().header.height),
                )
            }
        };
        container(column![header, body].height(Length::Fill))
            .width(shell::INSPECTOR_WIDTH)
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
        let rgba = |c: [u8; 4]| Color::from_rgba8(c[0], c[1], c[2], c[3] as f32 / 255.0);
        let mut rows = column![];
        for (index, item) in popup::COMMANDS.iter().enumerate() {
            if item.separator_before {
                rows = rows.push(container(Space::new().height(1)).width(Length::Fill).style(
                    move |_| container::Style {
                        background: Some(rgba(popup::BORDER).into()),
                        ..Default::default()
                    },
                ));
            }
            let selected = self.reference_tools_selection == Some(index);
            let fg = rgba(if item.placeholder {
                popup::MUTED
            } else {
                popup::FOREGROUND
            });
            let row = button(
                row![
                    vector_icon(Icon::from_key(item.icon_key), fg, popup::ICON_SIZE),
                    self.chrome_label(item.title, 10.0, false, false).color(fg),
                    Space::new().width(Length::Fill),
                    self.chrome_label(item.description, 9.0, true, false)
                        .color(rgba(popup::MUTED))
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            )
            .height(popup::ROW_HEIGHT)
            .width(Length::Fill)
            .padding([5, 7])
            .on_press_maybe((!item.placeholder).then_some(Message::ReferenceToolsChoose(index)))
            .style(move |_, status| button::Style {
                background: Some(
                    rgba(
                        if selected
                            || (!item.placeholder
                                && matches!(
                                    status,
                                    button::Status::Hovered | button::Status::Pressed
                                ))
                        {
                            popup::SELECTED
                        } else {
                            popup::SURFACE
                        },
                    )
                    .into(),
                ),
                text_color: fg,
                border: iced::Border {
                    radius: 6.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });
            rows = rows.push(mouse_area(row).on_enter(Message::ReferenceToolsSelect(index)));
        }
        let menu = container(rows)
            .width(popup::WIDTH)
            .padding(popup::PADDING)
            .style(move |_| container::Style {
                background: Some(rgba(popup::SURFACE).into()),
                border: iced::Border {
                    color: rgba(popup::BORDER),
                    width: 1.0,
                    radius: popup::RADIUS.into(),
                },
                shadow: iced::Shadow {
                    color: Color::from_rgba(0.02, 0.04, 0.08, 0.22),
                    offset: iced::Vector::new(0.0, 4.0),
                    blur_radius: 12.0,
                },
                ..Default::default()
            });
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
                    panel(
                        theme,
                        color::TERMINAL,
                        color::LIGHT_BORDER,
                        glass::PANEL_RADIUS,
                    )
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
            .height(glass::TOOLBAR_HEIGHT);
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
        body = body.push(Space::new().width(glass::LEFT_GAP));
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
            body = body.push(Space::new().width(glass::INSPECTOR_GAP));
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
                    top: glass::TOPBAR_HEIGHT,
                    ..iced::Padding::ZERO
                })
                .width(Length::Fill)
                .height(Length::Fill);
                let inspector = container(
                    self.font_zoom_region(font_zoom::Group::Chrome, self.reference_inspector()),
                )
                .padding(iced::Padding {
                    top: glass::MAIN_TOP,
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
