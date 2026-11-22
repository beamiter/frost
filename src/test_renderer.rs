//! Software rendering for headless widget tests in either Cargo profile.

pub(crate) use iced_tiny_skia::Renderer as HeadlessRenderer;

pub(crate) type InputState = iced::widget::text_input::State<
    <HeadlessRenderer as iced::advanced::text::Renderer>::Paragraph,
>;

pub(crate) fn software_renderer() -> HeadlessRenderer {
    let settings = iced::Settings::default();
    HeadlessRenderer::new(settings.default_font, settings.default_text_size)
}
