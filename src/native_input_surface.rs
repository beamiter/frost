//! One retained terminal widget, viewed through history and composer clips.
//! No editor, PTY input queue, cursor or IME state is duplicated.
/// Both keyboard/IME and application-mouse gates share this actual modal fact.
pub(crate) fn ai_surface_active(chats_open: bool, ask_open: bool) -> bool {
    chats_open || ask_open
}

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
    pub vertical_padding: f32,
}

// The raw native viewport always reserves the full outer composer. The outer
// Surface includes this input strip and the separate tools row is below it.
pub(crate) const INPUT_STRIP_HEIGHT: f32 = crate::native_viewport::INPUT_STRIP_HEIGHT;

pub(crate) struct DockMetrics {
    pub input_height: f32,
    pub vertical_padding: f32,
    pub visible_rows: usize,
}

/// Short prompts may use less decoration, never fewer native rows. Longer
/// drafts retain the original 8px padding and bounded scrolling behavior.
pub(crate) fn dock_metrics(rows: usize, cell_height: f32, cap: f32) -> DockMetrics {
    let rows = rows.max(1);
    let body_height = rows as f32 * cell_height;
    let vertical_padding = if rows <= 2 && body_height <= INPUT_STRIP_HEIGHT {
        ((INPUT_STRIP_HEIGHT - body_height) * 0.5).min(8.0)
    } else {
        8.0
    };
    let decorated_height = body_height + 2.0 * vertical_padding;
    let input_height = decorated_height.max(INPUT_STRIP_HEIGHT).min(cap);
    let visible_rows = if decorated_height <= input_height {
        // All requested rows fit. Avoid losing a row to a rounded float
        // division at the exact two-row boundary.
        rows
    } else {
        ((input_height - 2.0 * vertical_padding) / cell_height)
            .floor()
            .max(1.0) as usize
    };
    DockMetrics {
        input_height,
        vertical_padding,
        visible_rows,
    }
}

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
            (size.height - dock.input_height).max(0.0) + dock.vertical_padding - dock.y
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
                            color: crate::native_shell::composer_border_color(),
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
        let history_redraw_cursor = matches!(
            event,
            Event::Window(iced::window::Event::RedrawRequested(_))
        )
        .then(|| {
            cursor
                .position()
                .filter(|position| history_clip.contains(*position))
                .map_or(mouse::Cursor::Unavailable, mouse::Cursor::Available)
        });
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
        if let (false, Some(history)) = (input, self.history.as_mut()) {
            history.as_widget_mut().update(
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
        if input {
            if let (Some(history), Some(cursor)) = (self.history.as_mut(), history_redraw_cursor) {
                // Iced Scrollable commits its paint status on RedrawRequested.
                // Preserve the terminal update above, while refreshing retained
                // history hover/drag paint with its own real clip and cursor.
                // Keyboard, IME and every other event keep the exclusive route.
                history.as_widget_mut().update(
                    &mut tree.children[1],
                    event,
                    layout.child(2),
                    cursor,
                    renderer,
                    clipboard,
                    shell,
                    &history_clip,
                );
            }
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

#[cfg(test)]
mod tests {
    use super::{clips, dock_metrics};
    use iced::Rectangle;

    #[test]
    fn two_default_rows_fit_the_reference_strip_without_losing_a_row() {
        let metrics = dock_metrics(2, 16.8, 180.0);
        assert_eq!(metrics.input_height, 44.0);
        assert_eq!(metrics.visible_rows, 2);
        assert!(metrics.vertical_padding < 8.0);
        assert!(2.0 * 16.8 + 2.0 * metrics.vertical_padding <= metrics.input_height);
    }

    #[test]
    fn a_short_prompt_keeps_its_existing_padding() {
        let metrics = dock_metrics(1, 16.8, 180.0);
        assert_eq!(metrics.input_height, 44.0);
        assert_eq!(metrics.vertical_padding, 8.0);
        assert_eq!(metrics.visible_rows, 1);
    }

    #[test]
    fn two_row_float_boundaries_keep_both_cells_and_caret_inside_the_clip() {
        for cell_height in [14.4_f32, 16.2, 16.8, 17.1, 18.6, 21.6, 22.0] {
            let metrics = dock_metrics(2, cell_height, 180.0);
            assert_eq!(metrics.input_height, 44.0, "{cell_height}");
            assert_eq!(metrics.visible_rows, 2, "{cell_height}");
            let (_, clip) = clips(
                Rectangle {
                    x: 0.0,
                    y: 0.0,
                    width: 702.0,
                    height: 550.0,
                },
                metrics.input_height,
            );
            for row in 0..2 {
                let cell_top = clip.y + metrics.vertical_padding + row as f32 * cell_height;
                assert!(cell_top >= clip.y, "cell {row}/{cell_height}");
                assert!(
                    cell_top + cell_height <= clip.y + clip.height,
                    "cell/caret {row}/{cell_height}"
                );
            }
        }
    }

    #[test]
    fn larger_physical_cells_grow_instead_of_being_clipped() {
        let metrics = dock_metrics(2, 24.0, 180.0);
        assert_eq!(metrics.input_height, 64.0);
        assert_eq!(metrics.vertical_padding, 8.0);
        assert_eq!(metrics.visible_rows, 2);
    }

    #[test]
    fn multiline_and_capped_drafts_keep_the_original_budget_and_last_row() {
        let multiline = dock_metrics(4, 16.8, 180.0);
        assert_eq!(multiline.input_height, 4.0 * 16.8 + 16.0);
        assert_eq!(multiline.vertical_padding, 8.0);
        assert_eq!(multiline.visible_rows, 4);

        let capped = dock_metrics(100, 16.8, 180.0);
        assert_eq!(capped.input_height, 180.0);
        assert_eq!(capped.vertical_padding, 8.0);
        assert_eq!(capped.visible_rows, 9);
        let cursor = 99_usize;
        let visible_start = (cursor + 1).saturating_sub(capped.visible_rows);
        assert!(visible_start <= cursor && cursor < visible_start + capped.visible_rows);
    }
}
