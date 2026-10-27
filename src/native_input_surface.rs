//! One retained terminal widget, viewed through history and composer clips.
//! No editor, PTY input queue, cursor or IME state is duplicated.
use iced::advanced::widget::{tree, Tree};
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell, Widget};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

struct Surface<'a, Message> {
    terminal: Element<'a, Message>,
    history: Option<Element<'a, Message>>,
    dock: Option<Dock>,
    foreground_hint: String,
}

#[derive(Clone, Copy)]
pub(crate) struct Dock {
    pub y: f32,
    pub history_end_y: f32,
    pub input_height: f32,
}

// The raw native viewport always reserves the full outer composer. The outer
// Surface includes this input strip and the separate tools row is below it.
pub(crate) const INPUT_STRIP_HEIGHT: f32 = crate::native_viewport::INPUT_STRIP_HEIGHT;

impl<Message> Surface<'_, Message> {
    fn input_height(&self) -> f32 {
        self.dock
            .map_or(INPUT_STRIP_HEIGHT, |dock| dock.input_height)
    }
    fn input_layout_index(&self) -> usize {
        usize::from(self.dock.is_some())
    }
}

#[derive(Default)]
struct State {
    gesture: Option<bool>,
}

pub(crate) fn surface<'a, Message: 'a>(
    terminal: Element<'a, Message>,
    history: Option<Element<'a, Message>>,
    dock: Option<Dock>,
    foreground_hint: impl Into<String>,
) -> Element<'a, Message> {
    Element::new(Surface {
        terminal,
        history,
        dock,
        foreground_hint: foreground_hint.into(),
    })
}

fn clips(bounds: Rectangle, input_height: f32) -> (Rectangle, Rectangle) {
    let height = input_height.min(bounds.height).max(0.0);
    (
        Rectangle {
            height: bounds.height - height,
            ..bounds
        },
        Rectangle {
            y: bounds.y + bounds.height - height,
            height,
            ..bounds
        },
    )
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for Surface<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }
    fn children(&self) -> Vec<Tree> {
        let mut children = vec![Tree::new(self.terminal.as_widget())];
        if let Some(history) = &self.history {
            children.push(Tree::new(history.as_widget()));
        }
        children
    }
    fn diff(&self, tree: &mut Tree) {
        tree.children[0].diff(self.terminal.as_widget());
        if let Some(history) = &self.history {
            if tree.children.len() == 1 {
                tree.children.push(Tree::new(history.as_widget()));
            } else {
                tree.children[1].diff(history.as_widget());
            }
        } else {
            tree.children.truncate(1);
        }
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.max();
        let native_size = Size::new(
            size.width,
            crate::native_viewport::native_body_height(size.height),
        );
        let terminal = self.terminal.as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &layout::Limits::new(native_size, native_size),
        );
        log::debug!(target: "frost::reference_tools", "native_surface canonical={:?} outer={:?}", terminal.bounds(), size);
        let shift = self.dock.map_or(0.0, |dock| {
            (size.height - dock.input_height).max(0.0) + 8.0 - dock.y
        });
        // The canonical native child keeps this origin and stable raw height
        // through Prompt/Running/TUI. Only a proven prompt's dock paint/IME
        // layout is translated; no native child or input owner is recreated.
        let dock = terminal.clone().translate(Vector::new(0.0, shift));
        let mut nodes = vec![terminal, dock];
        let history_height = (size.height - self.input_height()).max(0.0);
        if let Some(history) = &mut self.history {
            let history_size = Size::new(size.width, history_height);
            nodes.push(history.as_widget_mut().layout(
                &mut tree.children[1],
                renderer,
                &layout::Limits::new(history_size, history_size),
            ));
        }
        layout::Node::with_children(size, nodes)
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let (history_clip, input_clip) = clips(layout.bounds(), self.input_height());
        if let Some(clip) = history_clip.intersection(viewport) {
            renderer.with_layer(clip, |renderer| {
                if let Some(history) = &self.history {
                    history.as_widget().draw(
                        &tree.children[1],
                        renderer,
                        theme,
                        style,
                        layout.child(2),
                        cursor,
                        &clip,
                    );
                } else {
                    // Exclude the old dock's complete native paint region.
                    // Otherwise its original caret could remain visible above
                    // the translated composer, despite there being one owner.
                    let native_clip = Rectangle {
                        height: clip.height.min(self.dock.map_or(clip.height, |dock| {
                            (layout.bounds().y + dock.history_end_y - clip.y).max(0.0)
                        })),
                        ..clip
                    };
                    if native_clip.height > 0.0 {
                        self.terminal.as_widget().draw(
                            &tree.children[0],
                            renderer,
                            theme,
                            style,
                            layout.child(0),
                            cursor,
                            &native_clip,
                        );
                    }
                }
            });
        }
        if let Some(clip) = input_clip.intersection(viewport) {
            renderer.with_layer(clip, |renderer| {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: input_clip,
                        border: iced::Border {
                            color: iced::Color::from_rgb8(210, 221, 239),
                            width: 1.0,
                            radius: iced::border::Radius {
                                top_left: 14.0,
                                top_right: 14.0,
                                bottom_left: 0.0,
                                bottom_right: 0.0,
                            },
                        },
                        ..Default::default()
                    },
                    iced::Background::Color(iced::Color::from_rgb8(254, 254, 254)),
                );
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            x: input_clip.x + 1.0,
                            y: input_clip.y + input_clip.height - 1.0,
                            width: (input_clip.width - 2.0).max(0.0),
                            height: 1.0,
                        },
                        ..Default::default()
                    },
                    iced::Background::Color(iced::Color::from_rgb8(254, 254, 254)),
                );
                if self.dock.is_some() {
                    let native_clip = Rectangle {
                        width: (clip.width - crate::terminal_view::SCROLLBAR_WIDTH - 4.0).max(0.0),
                        ..clip
                    };
                    self.terminal.as_widget().draw(
                        &tree.children[0],
                        renderer,
                        theme,
                        style,
                        layout.child(1),
                        cursor,
                        &native_clip,
                    );
                } else {
                    use iced::advanced::text::Renderer as _;
                    renderer.fill_text(
                        iced::advanced::text::Text {
                            content: self.foreground_hint.clone(),
                            bounds: Size::new((clip.width - 28.0).max(0.0), clip.height),
                            size: iced::Pixels(12.0),
                            line_height: iced::advanced::text::LineHeight::Absolute(iced::Pixels(
                                16.0,
                            )),
                            font: iced::Font::with_name(jterm_core::reference_shell::FONT_SANS),
                            align_x: iced::advanced::text::Alignment::Left,
                            align_y: iced::alignment::Vertical::Center,
                            shaping: iced::advanced::text::Shaping::Advanced,
                            wrapping: iced::advanced::text::Wrapping::None,
                        },
                        iced::Point::new(clip.x + 14.0, clip.y + clip.height / 2.0),
                        iced::Color::from_rgb8(111, 125, 148),
                        clip,
                    );
                }
            });
        }
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        let input_layout = self.input_layout_index();
        self.terminal.as_widget_mut().operate(
            &mut tree.children[0],
            layout.child(input_layout),
            renderer,
            operation,
        );
        if let Some(history) = &mut self.history {
            history.as_widget_mut().operate(
                &mut tree.children[1],
                layout.child(2),
                renderer,
                operation,
            );
        }
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let (history_clip, input_clip) = clips(layout.bounds(), self.input_height());
        let input_hit = cursor.position_over(input_clip).is_some();
        let history_hit = cursor.position_over(history_clip).is_some();
        let pointer_event = matches!(event, Event::Mouse(_) | Event::Touch(_));
        let state = tree.state.downcast_mut::<State>();
        if matches!(event, Event::Mouse(mouse::Event::ButtonPressed(_)))
            && (input_hit || history_hit)
        {
            state.gesture = Some(input_hit);
        }
        let input = self.dock.is_some()
            && if pointer_event {
                if input_hit {
                    true
                } else if history_hit {
                    false
                } else {
                    state.gesture.unwrap_or(false)
                }
            } else {
                true
            };
        let clip = if input {
            Rectangle {
                width: (input_clip.width - crate::terminal_view::SCROLLBAR_WIDTH - 4.0).max(0.0),
                ..input_clip
            }
        } else {
            history_clip
        };
        let cursor = if let Some(position) = cursor.position().filter(|p| clip.contains(*p)) {
            mouse::Cursor::Available(position)
        } else {
            mouse::Cursor::Unavailable
        };
        if !input && self.history.is_some() {
            self.history.as_mut().unwrap().as_widget_mut().update(
                &mut tree.children[1],
                event,
                layout.child(2),
                cursor,
                renderer,
                clipboard,
                shell,
                &clip,
            );
        } else {
            // Redraw/IME requests always use the composer presentation layout.
            // InputMethod cursor rectangles therefore share the exact transform
            // used by native caret, preedit paint and pointer hit testing.
            self.terminal.as_widget_mut().update(
                &mut tree.children[0],
                event,
                layout.child(if input { 1 } else { 0 }),
                cursor,
                renderer,
                clipboard,
                shell,
                &clip,
            );
        }
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonReleased(_))
                | Event::Window(iced::window::Event::Unfocused)
        ) {
            tree.state.downcast_mut::<State>().gesture = None;
        }
        let _ = viewport;
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let (history_clip, input_clip) = clips(layout.bounds(), self.input_height());
        if cursor.position_over(input_clip).is_some() && self.dock.is_none() {
            mouse::Interaction::default()
        } else if cursor.position_over(input_clip).is_some() {
            self.terminal.as_widget().mouse_interaction(
                &tree.children[0],
                layout.child(1),
                cursor,
                &input_clip,
                renderer,
            )
        } else if let Some(history) = &self.history {
            history.as_widget().mouse_interaction(
                &tree.children[1],
                layout.child(2),
                cursor,
                &history_clip,
                renderer,
            )
        } else {
            self.terminal.as_widget().mouse_interaction(
                &tree.children[0],
                layout.child(0),
                cursor,
                &history_clip,
                renderer,
            )
        }
    }
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, iced::Theme, iced::Renderer>> {
        let input_layout = self.input_layout_index();
        self.terminal.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.child(input_layout),
            renderer,
            viewport,
            translation,
        )
    }
}
