//! Presentation-only layers keep the native input subtree at index zero.
use iced::{widget::stack, Element};
pub(crate) fn layers<'a, Message: 'a>(
    native: Element<'a, Message>,
    dismiss: Element<'a, Message>,
    popup: Element<'a, Message>,
) -> Element<'a, Message> {
    stack![native, dismiss, popup].into()
}
