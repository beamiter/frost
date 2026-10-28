//! Opt-in native QA measurements of real shaped Iced text, never user content.
use iced::advanced::text::Paragraph as _;
use iced::advanced::widget::{tree, Tree};
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell, Widget};
use iced::{Element, Event, Length, Rectangle, Size, Vector};
use std::path::PathBuf;

type Paragraph = iced::advanced::graphics::text::Paragraph;
type TextState = iced::widget::text::State<Paragraph>;

pub(crate) fn observe<'a, Message: 'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    if !jterm_core::reference_conversation::fixture_enabled() {
        return content;
    }
    let Some(path) = std::env::var_os("JTERM_NATIVE_FONT_PROBE").map(PathBuf::from) else {
        return content;
    };
    Element::new(Probe { content, path })
}

struct Probe<'a, Message> {
    content: Element<'a, Message>,
    path: PathBuf,
}
#[derive(Default)]
struct State(String);

fn collect(tree: &Tree, rows: &mut Vec<serde_json::Value>) {
    if tree.tag == tree::Tag::of::<TextState>() {
        let paragraph = tree.state.downcast_ref::<TextState>().raw();
        let value = paragraph
            .buffer()
            .lines
            .iter()
            .map(|line| line.text())
            .collect::<Vec<_>>()
            .join("\n");
        // Deliberately exact, fixed sample strings: no terminal, path, task,
        // credential, or other production text can enter the report.
        if [
            "Frost",
            "Workspace / Canvas renderer",
            "Canvas renderer",
            "Agent",
            "MODEL",
            "WORKSPACE",
            "BRANCH",
            "Local session",
            "Attach",
            "Model unavailable",
            "Assistant",
            "One continuous canvas, a stable reading position",
            "The message surface keeps your place as new output arrives. Navigation, context and tools remain together around the canvas.",
            "Update the canvas renderer with retained scroll and an infinite canvas.",
            "Read files",
            "Inspect the current renderer",
            "src/canvas.rs",
            "38",
            "39",
            "// Viewport positions use logical pixels.",
        ]
        .contains(&value.as_str())
        {
            let bounds = paragraph.min_bounds();
            rows.push(serde_json::json!({"text": value, "font_size": paragraph.size().0,
                "shaped_bounds": [bounds.width, bounds.height],
                "glyph_count": paragraph.buffer().layout_runs().map(|run| run.glyphs.len()).sum::<usize>()}));
        }
    }
    for child in &tree.children {
        collect(child, rows);
    }
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for Probe<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.content.as_widget())]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.children[0].diff(self.content.as_widget());
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let node = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let mut rows = Vec::new();
        collect(&tree.children[0], &mut rows);
        rows.sort_by_cached_key(serde_json::Value::to_string);
        let encoded = serde_json::json!({"source": "native Iced Text paragraph after actual renderer layout", "paragraphs": rows}).to_string();
        let previous = &mut tree.state.downcast_mut::<State>().0;
        if *previous != encoded {
            let temporary = self.path.with_extension("tmp");
            if std::fs::write(&temporary, &encoded)
                .and_then(|_| std::fs::rename(&temporary, &self.path))
                .is_ok()
            {
                *previous = encoded;
            }
        }
        node
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
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
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, iced::Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
