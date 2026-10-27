//! Standalone Iced tree-retention contract. Uses the production Surface; the
//! scrollbar shim supplies one geometry constant; viewport allocation is the
//! actual production module used by the application.
use iced::advanced::{layout, mouse, renderer, Layout, Widget};
use iced::advanced::widget::{tree, Tree};
use iced::{Element, Length, Rectangle, Size};
use std::{cell::Cell, rc::Rc};
#[path = "../src/native_viewport.rs"]
mod native_viewport;
mod terminal_view { pub const SCROLLBAR_WIDTH: f32 = 10.0; }
#[path = "../src/native_input_surface.rs"]
mod native_input_surface;
#[path = "../src/native_popup_surface.rs"]
mod native_popup_surface;

struct NativeProbe(Rc<Cell<usize>>);
struct NativeState(u64);
impl Widget<(), iced::Theme, iced::Renderer> for NativeProbe {
    fn size(&self) -> Size<Length> { Size::new(Length::Fill, Length::Fill) }
    fn tag(&self) -> tree::Tag { tree::Tag::of::<NativeState>() }
    fn state(&self) -> tree::State {
        self.0.set(self.0.get() + 1);
        tree::State::new(NativeState(0))
    }
    fn layout(&mut self, _: &mut Tree, _: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.max())
    }
    fn draw(&self, _: &Tree, _: &mut iced::Renderer, _: &iced::Theme,
            _: &renderer::Style, _: Layout<'_>, _: mouse::Cursor, _: &Rectangle) {}
}
fn surface(births: Rc<Cell<usize>>, prompt: bool, history: bool, input_height: f32) -> Element<'static, ()> {
    native_input_surface::surface(
        Element::new(NativeProbe(births)),
        history.then(|| iced::widget::Space::new().into()),
        prompt.then_some(native_input_surface::Dock { y: 448.0, history_end_y: 448.0, input_height }),
        "Foreground input remains in the native terminal",
    )
}
#[test]
fn prompt_foreground_tui_and_return_retain_one_native_state() {
    let births = Rc::new(Cell::new(0));
    let prompt = surface(births.clone(), true, true, 44.0);
    let mut tree = Tree::new(prompt.as_widget());
    tree.children[0].state.downcast_mut::<NativeState>().0 = 0xfeed;
    for (prompt, history, height) in [(false, false, 44.0), (false, false, 44.0), (true, false, 153.0), (true, true, 180.0), (true, true, 44.0)] {
        let next = surface(births.clone(), prompt, history, height);
        tree.diff(next.as_widget());
        assert_eq!(tree.children[0].state.downcast_ref::<NativeState>().0, 0xfeed,
                   "foreground transition recreated the native input state");
        assert_eq!(births.get(), 1, "a second native state was constructed");
        assert_eq!(tree.children.iter().filter(|child| child.tag == tree::Tag::of::<NativeState>()).count(), 1);
    }
}

#[test]
fn tools_popup_open_close_retains_the_same_native_owner() {
    let births = Rc::new(Cell::new(0));
    let wrap = |open: bool| native_popup_surface::layers(
        surface(births.clone(), true, false, 44.0),
        iced::widget::Space::new().width(if open { Length::Fill } else { Length::Fixed(0.0) }).into(),
        if open { iced::widget::button(iced::widget::text("Tools")).on_press(()).into() }
        else { iced::widget::Space::new().into() },
    );
    let first = wrap(false);
    let mut tree = Tree::new(first.as_widget());
    tree.children[0].children[0].state.downcast_mut::<NativeState>().0 = 0xfeed;
    for open in [true, false, true, false] {
        let next = wrap(open);
        tree.diff(next.as_widget());
        assert_eq!(tree.children[0].children[0].state.downcast_ref::<NativeState>().0, 0xfeed);
        assert_eq!(births.get(), 1, "Tools constructed a second native owner");
    }
}
