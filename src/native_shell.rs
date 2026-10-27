//! Native Iced binding for the family's measured workspace contract.
//! The terminal widget remains the sole PTY, selection and IME input owner.

use super::*;
use crate::reference_icon::{icon as vector_icon, Icon};
use iced::widget::{column, opaque};
use jterm_core::reference_shell::{self as shell, color};

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
    Color::from_rgb8(value[0], value[1], value[2])
}

fn panel(background: [u8; 3], border: [u8; 3], radius: f32) -> container::Style {
    container::Style {
        background: Some(rgb(background).into()),
        text_color: Some(rgb(color::CHROME_TEXT)),
        border: iced::Border {
            color: rgb(border),
            width: 1.0,
            radius: radius.into(),
        },
        ..Default::default()
    }
}

fn control(
    selected: bool,
    light: bool,
) -> impl Fn(&iced::Theme, button::Status) -> button::Style + 'static {
    move |_, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let disabled = matches!(status, button::Status::Disabled);
        button::Style {
            background: (selected || hovered).then(|| {
                rgb(if selected {
                    if light {
                        color::ACCENT_PALE
                    } else {
                        color::NAVIGATION_SELECTED
                    }
                } else if light {
                    color::ACCENT_PALE
                } else {
                    color::INSPECTOR_CARD
                })
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
                radius: shell::CONTROL_RADIUS.into(),
                ..Default::default()
            },
            ..Default::default()
        }
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

fn section(value: &'static str) -> Element<'static, Message> {
    container(label(value, 9.0, true, false))
        .padding([4, 0])
        .into()
}

fn short(value: &str, count: usize) -> String {
    let mut result: String = value.chars().take(count).collect();
    if value.chars().count() > count {
        result.push('…');
    }
    result
}

impl Frost {
    pub(crate) fn reference_layout(&self) -> shell::Layout {
        let mut layout = shell::Layout::for_size(self.win_size.width, self.win_size.height);
        if !self.sidebar_open {
            if let Some(nav) = layout.navigation.take() {
                layout.center.x -= nav.width;
                layout.center.width += nav.width;
            }
        }
        if !self.reference_inspector_open {
            if let Some(inspector) = layout.inspector.take() {
                layout.center.width += inspector.width;
            }
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
            if config.is_chat_mode() {
                theme.terminal = Theme::builtin_light().terminal;
                theme.terminal.background = color::CANVAS;
                theme.terminal.foreground = color::CANVAS_TEXT;
                theme.terminal.cursor = color::ACCENT;
                // ANSI bright variants must stay readable on the pale canvas.
                // Explicit user palettes and native Block colors are retained.
                theme.terminal.ansi_colors[1] = [173, 49, 66];
                theme.terminal.ansi_colors[9] = [190, 52, 68];
                theme.terminal.ansi_colors[2] = [29, 112, 65];
                theme.terminal.ansi_colors[10] = [24, 125, 70];
            } else {
                theme.terminal.background = color::TERMINAL;
            }
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
        button(label(value, 11.0, false, light))
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
        button(container(vector_icon(glyph, rgb(color::CHROME_TEXT), 15.0)).center(28))
            .on_press(action)
            .width(28)
            .height(28)
            .padding(0)
            .style(control(selected, false))
    }

    pub(crate) fn reference_topbar(&self) -> Element<'_, Message> {
        let window_control = |glyph: &'static str, action: Message| {
            button(container(label(glyph, 12.0, false, false)).center(24))
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
        .align_y(iced::Alignment::Center);
        let identity = row![
            crate::reference_icon::identity(
                "frost",
                jterm_core::reference_identity::PRODUCT_ICON_SIZE
            ),
            label(
                if self.win_size.width >= 560.0 {
                    "Frost"
                } else {
                    ""
                },
                12.0,
                false,
                false
            ),
        ]
        .spacing(7)
        .align_y(iced::Alignment::Center);
        let path = self.reference_cwd();
        let path: Element<'_, Message> = if self.win_size.width >= 1050.0 {
            tooltip(
                container(label(format!("⌁  {}", short(&path, 28)), 10.0, true, false))
                    .width(Length::Fixed(205.0))
                    .clip(true),
                label(path, 11.0, false, false),
                tooltip::Position::Bottom,
            )
            .into()
        } else {
            Space::new().into()
        };
        let search: Element<'_, Message> = if self.win_size.width >= 850.0 {
            button(
                row![
                    vector_icon(Icon::Search, rgb(color::CHROME_MUTED), 14.0),
                    label("Search or run a command…", 10.0, true, false),
                    Space::new().width(Length::Fill),
                    container(label("Ctrl ⇧ P", 9.0, true, false))
                        .padding([2, 4])
                        .style(|_| panel(color::TITLEBAR, color::DARK_BORDER, 4.0))
                ]
                .align_y(iced::Alignment::Center),
            )
            .on_press(Message::ReferencePalette)
            .width(Length::Fixed(if self.win_size.width >= 1250.0 {
                362.0
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
        let search_width = if self.win_size.width >= 1250.0 {
            shell::SEARCH_WIDTH
        } else if self.win_size.width >= 850.0 {
            230.0
        } else {
            28.0
        };
        let search_x = (self.win_size.width - search_width) * 0.5 + shell::SEARCH_CENTER_OFFSET;
        let left_width = (search_x - 14.0)
            .max(106.0)
            .min((self.win_size.width - search_width - 148.0).max(106.0));
        let left = row![controls, identity, path]
            .spacing(7)
            .align_y(iced::Alignment::Center)
            .width(left_width);
        let mut right = row![
            Space::new().width(Length::Fill),
            self.shell_button(
                "Block",
                Message::SetTerminalMode(config::TerminalMode::Block),
                mode != config::TerminalMode::Chat,
                false
            ),
            self.shell_button(
                "Chat",
                Message::SetTerminalMode(config::TerminalMode::Chat),
                mode == config::TerminalMode::Chat,
                false
            ),
            self.vector_button(
                Icon::Grid,
                Message::ReferenceInspectorToggle,
                self.reference_inspector_open
            ),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center)
        .width(Length::Fill);
        if self.win_size.width >= 500.0 {
            right =
                right.push(self.vector_button(Icon::Settings, Message::ToggleConfigPanel, false));
        }
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
        .style(|_| panel(color::TITLEBAR, color::DARK_BORDER, 0.0))
        .into()
    }

    fn reference_rail(&self) -> Element<'_, Message> {
        let entry = |glyph: Icon,
                     hint: &'static str,
                     action: Message,
                     selected: bool|
         -> Element<'_, Message> {
            tooltip(
                button(container(vector_icon(glyph, rgb(color::CHROME_TEXT), 18.0)).center(34))
                    .on_press(action)
                    .width(34)
                    .height(34)
                    .padding(0)
                    .style(control(selected, false)),
                label(hint, 11.0, false, false),
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
                    container(label(initial, 12.0, false, false).center())
                        .width(shell::PROFILE_DIAMETER)
                        .height(shell::PROFILE_DIAMETER)
                        .center_y(shell::PROFILE_DIAMETER)
                        .style(|_| {
                            panel(
                                color::PROFILE_BACKGROUND,
                                color::PROFILE_BORDER,
                                shell::PROFILE_DIAMETER / 2.0,
                            )
                        }),
                ),
        )
        .padding([14, 0])
        .width(shell::RAIL_WIDTH)
        .height(Length::Fill)
        .style(|_| panel(color::RAIL, color::DARK_BORDER, 0.0))
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
                label(short(branch, 6), 8.0, true, false).wrapping(text::Wrapping::None)
            ]
            .spacing(3)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            label("Local", 8.0, true, false).into()
        };
        let selector = tooltip(
            container(
                row![
                    vector_icon(Icon::Files, rgb(color::CHROME_MUTED), 14.0),
                    label(
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
                    .wrapping(text::Wrapping::None),
                    container(branch_slot).width(49).clip(true),
                    vector_icon(Icon::ChevronDown, rgb(color::CHROME_MUTED), 13.0)
                ]
                .spacing(7)
                .align_y(iced::Alignment::Center),
            )
            .height(shell::WORKSPACE_SELECTOR_HEIGHT)
            .padding([6, 9])
            .style(|_| panel(color::INSPECTOR_CARD, color::DARK_BORDER, 8.0)),
            label(format!("{cwd} · {branch_metadata}"), 10.0, false, false),
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
                        label(title, 10.0, false, false)
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
                                label(item.title, 10.0, false, false),
                                label(item.detail, 8.0, true, false)
                            ]
                            .spacing(1)
                            .width(Length::Fill),
                            label(item.time, 8.0, true, false)
                        ]
                        .spacing(7)
                        .align_y(iced::Alignment::Center),
                    )
                    .height(shell::RECENT_ROW_HEIGHT)
                    .padding([3, 7])
                    .style(move |_| {
                        panel(
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
                        label(short(&self.tab_label(index), 22), 10.0, false, false)
                            .width(Length::Fill)
                    ]
                    .spacing(7),
                )
                .height(shell::RECENT_ROW_HEIGHT)
                .padding([7, 8])
                .style(move |_| {
                    panel(
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
                    label(self.tab_label(index), 11.0, false, false),
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
                        crate::reference_icon::identity(
                            name,
                            jterm_core::reference_identity::SPACE_ICON_SIZE
                        ),
                        column![
                            label(*name, 10.0, false, false),
                            label(
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
                .height(shell::SPACES_ROW_HEIGHT)
                .padding([2, 6]),
            );
        }
        let navigation = column![
            Space::new().height(shell::NAVIGATION_LABEL_TOP - shell::TITLEBAR_HEIGHT),
            container(label("WORKSPACE", 9.0, true, false))
                .height(shell::WORKSPACE_SELECTOR_TOP - shell::NAVIGATION_LABEL_TOP),
            selector,
            Space::new().height(
                shell::NAVIGATION_ITEMS_TOP
                    - shell::WORKSPACE_SELECTOR_TOP
                    - shell::WORKSPACE_SELECTOR_HEIGHT
            ),
            destinations,
            Space::new().height(8),
            container(section("RECENT")).height(20),
            container(recent).height(177),
            Space::new().height(7),
            container(section("SPACES")).height(20),
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
                        container(Space::new()).width(5).height(5).style(|_| panel(
                            color::CHROME_MUTED,
                            color::CHROME_MUTED,
                            3.0
                        )),
                        label("Local session", 10.0, false, false)
                    ]
                    .spacing(7)
                    .align_y(iced::Alignment::Center),
                    label(status, 8.0, true, false)
                ]
                .spacing(0)
            )
            .height(shell::NAVIGATION_STATUS_HEIGHT)
            .width(Length::Fill)
        ])
        .padding([0.0, shell::NAVIGATION_PADDING])
        .width(shell::NAVIGATION_WIDTH)
        .height(Length::Fill)
        .style(|_| panel(color::NAVIGATION, color::DARK_BORDER, 0.0))
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
            label(short(&name, 55), 12.0, false, true).font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..iced::Font::with_name(shell::FONT_SANS)
            }),
            label(
                if jterm_core::reference_conversation::fixture_enabled() {
                    jterm_core::reference_conversation::FIXTURE_LABEL.to_string()
                } else {
                    format!("{mode}  ·  Local session")
                },
                9.0,
                true,
                true
            )
        ]
        .spacing(3);
        container(
            row![
                heading,
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
        .height(shell::CANVAS_HEADER_HEIGHT)
        .padding([6.0, shell::COMPOSER_INSET])
        .align_y(iced::Alignment::Center)
        .style(|_| {
            let mut style = panel(color::CANVAS, color::LIGHT_BORDER, 0.0);
            style.text_color = Some(rgb(color::CANVAS_TEXT));
            style
        })
        .into()
    }

    pub(crate) fn reference_tools_height(&self) -> f32 {
        if self.config.is_chat_mode() {
            TOOLS_HEIGHT
        } else {
            shell::COMPOSER_HEIGHT
        }
    }

    fn reference_tools(&self) -> Element<'_, Message> {
        let tool = |glyph: Icon,
                    title: &'static str,
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
                contents = contents.push(vector_icon(
                    glyph,
                    rgb(fg),
                    jterm_core::reference_popup::FOOTER_ICON_SIZE,
                ));
            }
            let caption = label(
                title,
                jterm_core::reference_popup::FOOTER_LABEL_SIZE,
                !enabled,
                true,
            )
            .color(rgb(fg));
            if is_model {
                contents = contents
                    .push(caption.width(Length::Fill))
                    .push(vector_icon(Icon::ChevronDown, rgb(fg), 11.0))
                    .spacing(4);
            } else {
                contents = contents.push(caption);
                if title == jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[4] {
                    contents = contents.push(vector_icon(Icon::ChevronDown, rgb(fg), 11.0));
                }
            }
            let button = button(contents)
                .on_press_maybe(action)
                .height(shell::CONTROL_HEIGHT)
                .width(if is_model {
                    Length::Fixed(shell::COMPOSER_MODEL_MIN_WIDTH)
                } else {
                    Length::Shrink
                })
                .padding([4.0, jterm_core::reference_popup::FOOTER_HORIZONTAL_PADDING])
                .style(move |_, status| {
                    let mut style = control(false, true)(&iced::Theme::Light, status);
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
                    style.text_color = rgb(fg);
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
                    label("Not available yet", 10.0, true, true),
                    tooltip::Position::Top,
                )
                .into()
            }
        };
        let mut tools = row![button(
            container(vector_icon(Icon::Plus, rgb(color::CANVAS_TEXT), 18.0)).center(28)
        )
        .on_press(Message::NewSession)
        .width(28)
        .height(28)
        .padding(0)
        .style(control(false, true))]
        .spacing(shell::METADATA_GAP)
        .align_y(iced::Alignment::Center);
        if self.reference_layout().center.width >= 640.0 {
            tools = tools
                .push(tool(
                    Icon::Attach,
                    jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[0],
                    None,
                ))
                .push(tool(
                    Icon::Image,
                    jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[1],
                    None,
                ))
                .push(tool(
                    Icon::Code,
                    jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[2],
                    None,
                ));
        }
        tools = tools
            .push(tool(
                Icon::Terminal,
                jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[3],
                Some(Message::ReferenceFocusTerminal),
            ))
            .push(tool(
                Icon::Grid,
                jterm_core::reference_conversation::COMPOSER_TOOL_NAMES[4],
                Some(Message::ReferenceToolsToggle),
            ))
            .push(Space::new().width(Length::Fill));
        if self.reference_layout().center.width >= 650.0 {
            tools = tools.push(tool(
                Icon::ChevronDown,
                shell::MODEL_UNAVAILABLE_LABEL,
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
            16.0,
        ))
        .center(28)
        .width(28)
        .height(28)
        .style(move |_| {
            panel(
                if enabled {
                    color::ACCENT
                } else {
                    color::LIGHT_BORDER
                },
                color::LIGHT_BORDER,
                7.0,
            )
        });
        tools = tools.push(tooltip(
            chat_mode::press_control(arrow, action),
            label(
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
        let tools: Element<'_, Message> = container(tools)
            .padding(iced::Padding {
                top: 0.0,
                right: shell::COMPOSER_CONTROL_RIGHT,
                bottom: shell::COMPOSER_CONTROL_BOTTOM,
                left: shell::COMPOSER_CONTROL_LEFT,
            })
            .height(TOOLS_HEIGHT)
            .align_y(iced::Alignment::Center)
            .into();
        let content: Element<'_, Message> = if self.config.is_chat_mode() {
            tools
        } else {
            column![
                container(label(
                    "Type in the terminal · Enter to run",
                    12.0,
                    true,
                    true
                ))
                .padding([10, 14])
                .height(44),
                tools
            ]
            .into()
        };
        container(content)
            .height(self.reference_tools_height())
            .style(move |_| {
                let mut style = panel(color::COMPOSER, color::LIGHT_BORDER, shell::COMPOSER_RADIUS);
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
            button(label(name, 10.0, false, false))
                .on_press(Message::ReferenceInspector(value))
                .width(shell::INSPECTOR_TAB_WIDTH)
                .height(shell::CONTROL_HEIGHT)
                .padding([4, 5])
                .style(control(self.reference_inspector_tab == value, false))
        };
        let header = container(
            row![
                tab("Agent", InspectorTab::Agent),
                tab("Context", InspectorTab::Context),
                tab("Files", InspectorTab::Files),
                Space::new().width(Length::Fill),
                label("⊞", 13.0, true, false)
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        )
        .height(shell::CANVAS_HEADER_HEIGHT)
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
                        label("SESSION CONTEXT", 9.0, true, false),
                        label(cwd, 12.0, false, false),
                        label(git, 10.0, true, false),
                        label(
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
                self.sidebar_tasks_view()
            }
            InspectorTab::Agent => {
                let fixture = jterm_core::reference_conversation::fixture_enabled();
                let inspector = if fixture {
                    jterm_core::reference_conversation::acceptance_fixture().inspector
                } else {
                    crate::reference_conversation::observed_inspector(self)
                };
                crate::reference_conversation::inspector_view(&inspector, fixture)
            }
        };
        container(column![header, body].height(Length::Fill))
            .width(shell::INSPECTOR_WIDTH)
            .height(Length::Fill)
            .style(|_| panel(color::INSPECTOR, color::DARK_BORDER, 0.0))
            .into()
    }

    pub(crate) fn reference_terminal_surface<'a>(
        &'a self,
        terminal: Element<'a, Message>,
    ) -> Element<'a, Message> {
        if !self.config.is_chat_mode() {
            return terminal;
        }
        macro_rules! foreground {
            ($hint:expr) => {
                crate::native_input_surface::surface(terminal, None, None, $hint)
            };
        }
        let Some(session) = self.sessions.get(self.active) else {
            return foreground!("No active terminal session");
        };
        if self.is_split() {
            return foreground!("Input remains in the focused terminal pane");
        }
        if self.vim9_tabs.contains_key(&session.id) {
            return foreground!("Input remains in the editor");
        }
        let Some(dock) = session
            .projection
            .chat_rows()
            .iter()
            .position(|row| row.role == crate::chat_mode::ChatRole::Composer)
        else {
            return foreground!(match session.terminal.chat_surface() {
                crate::chat_mode::ChatSurface::Running =>
                    "Command running · Input remains in the terminal",
                crate::chat_mode::ChatSurface::Fullscreen =>
                    "Foreground application · Input remains in the terminal",
                _ => "Input remains in the terminal",
            });
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
        let cap = (self.term_height() * 0.45).clamp(44.0, 180.0);
        let input_height = (needed_rows as f32 * self.metrics.cell_h + 16.0)
            .max(44.0)
            .min(cap);
        let visible_rows = ((input_height - 16.0) / self.metrics.cell_h)
            .floor()
            .max(1.0) as usize;
        // Very long drafts scroll inside this bounded native input surface.
        // Pick an origin that includes the actual caret, rather than clipping
        // the first two rows while an IME edits below them.
        let visible_start = dock.max(
            session
                .cursor
                .0
                .saturating_add(1)
                .saturating_sub(visible_rows),
        );
        let dock_y = self.metrics.padding + visible_start as f32 * self.metrics.cell_h - margin;
        let history = jterm_core::reference_conversation::fixture_enabled().then(|| {
            crate::reference_conversation::conversation_view(
                &jterm_core::reference_conversation::acceptance_fixture(),
            )
        });
        crate::native_input_surface::surface(
            terminal,
            history,
            Some(crate::native_input_surface::Dock {
                y: dock_y,
                history_end_y: self.metrics.padding + dock as f32 * self.metrics.cell_h - margin,
                input_height,
            }),
            "",
        )
    }

    pub(crate) fn trace_reference_tools(&self, event: &str) {
        if let Some(session) = self.sessions.get(self.active) {
            log::debug!(target: "frost::reference_tools", "{event} open={} raw={:?} position_rows={} following={} projection={:?}",
                self.reference_tools_selection.is_some(), crate::native_viewport::raw_rect(self.reference_layout().center,
                    self.block_reading_bar_height(), self.startup_recovery.banner_height(self.chrome_font_size())),
                session.transcript_canvas.position_rows(), session.transcript_canvas.following(), session.projection.key());
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
                    label(item.title, 10.0, false, false).color(fg),
                    Space::new().width(Length::Fill),
                    label(item.description, 9.0, true, false).color(rgba(popup::MUTED))
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
        let top = (self.win_size.height
            - shell::COMPOSER_BOTTOM
            - shell::COMPOSER_CONTROL_BOTTOM
            - shell::CONTROL_HEIGHT
            - 8.0
            - height)
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
        let layout = self.reference_layout();
        let terminal = container(terminal)
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true)
            .style(move |_| {
                let mut style = panel(
                    if self.config.is_chat_mode() {
                        color::CANVAS
                    } else {
                        color::TERMINAL
                    },
                    if self.config.is_chat_mode() {
                        color::CANVAS
                    } else {
                        color::LIGHT_BORDER
                    },
                    shell::CARD_RADIUS,
                );
                style.text_color = Some(rgb(if self.config.is_chat_mode() {
                    color::CANVAS_TEXT
                } else {
                    color::CHROME_TEXT
                }));
                style
            });
        let center = container(
            column![
                self.reference_header(),
                container(column![terminal, self.reference_tools()].spacing(0))
                    .padding(iced::Padding {
                        top: CONTENT_TOP,
                        right: shell::COMPOSER_INSET,
                        bottom: shell::COMPOSER_BOTTOM,
                        left: shell::COMPOSER_INSET
                    })
                    .height(Length::Fill)
            ]
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| {
            let mut style = panel(color::CANVAS, color::LIGHT_BORDER, 0.0);
            style.text_color = Some(rgb(color::CANVAS_TEXT));
            style
        });
        let mut body = row![self.reference_rail()].height(Length::Fill);
        if layout.navigation.is_some() {
            body = body.push(self.reference_navigation());
        }
        body = body.push(center);
        if layout.inspector.is_some() {
            body = body.push(self.reference_inspector());
        }
        let workspace: Element<'_, Message> = column![self.reference_topbar(), body]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let workspace: Element<'_, Message> =
            if layout.inspector.is_none() && self.reference_inspector_drawer {
                let dismiss = button(
                    container(Space::new())
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .on_press(Message::ReferenceInspectorToggle)
                .padding(0)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_, _| button::Style {
                    background: Some(Color::from_rgba(0.05, 0.08, 0.14, 0.24).into()),
                    ..Default::default()
                });
                let inspector = container(self.reference_inspector())
                    .padding(iced::Padding {
                        top: shell::TITLEBAR_HEIGHT,
                        ..iced::Padding::ZERO
                    })
                    .align_right(Length::Fill)
                    .height(Length::Fill);
                stack![workspace, dismiss, inspector].into()
            } else {
                workspace
            };
        // Fixed Stack/base position in both states retains every native child.
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
        crate::native_popup_surface::layers(workspace, dismiss, menu)
    }
}
