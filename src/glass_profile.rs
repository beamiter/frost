//! Native Frost binding for the measured glass profile. No old core mutation.
pub use crate::glass_layout::Layout;
use crate::glass_shell_tokens as glass;
pub use jterm_core::reference_shell::{
    type_size, ASSISTANT_AVATAR_INSET, ASSISTANT_CONTENT_RIGHT, CONVERSATION_TOP_PADDING,
    DIFF_BODY_VERTICAL_PADDING, DIFF_GUTTER_COLUMN_WIDTH, DIFF_HEADER_HEIGHT, DIFF_LINE_HEIGHT,
    DIFF_SIGN_WIDTH, FONT_MONOSPACE, FONT_SANS, INSPECTOR_BADGE_FONT_SIZE, INSPECTOR_BADGE_HEIGHT,
    INSPECTOR_BADGE_HORIZONTAL_PADDING, INSPECTOR_BADGE_RADIUS, INSPECTOR_BADGE_VERTICAL_PADDING,
    INSPECTOR_OUTPUT_BOTTOM, INSPECTOR_QUEUE_GAP, MESSAGE_PART_GAP, METADATA_PILL_HEIGHT,
    MODEL_UNAVAILABLE_LABEL, NAVIGATION_PADDING, NO_PROGRESS_LABEL, TASK_ROW_HEIGHT,
    USER_BUBBLE_HEIGHT, USER_BUBBLE_RIGHT, USER_BUBBLE_WIDTH,
};

pub const TITLEBAR_HEIGHT: f32 = glass::TOPBAR_HEIGHT;
pub const INSPECTOR_WIDTH: f32 = glass::INSPECTOR_WIDTH;
pub const CARD_RADIUS: f32 = glass::PANEL_RADIUS;
pub const COMPOSER_RADIUS: f32 = glass::PANEL_RADIUS;
pub const CONTROL_RADIUS: f32 = glass::CONTROL_RADIUS;
pub const CONTROL_HEIGHT: f32 = glass::TOOL_BUTTON_HEIGHT;
pub const COMPOSER_INSET: f32 = glass::TERMINAL_INSET_LEFT;
pub const COMPOSER_CONTROL_LEFT: f32 = 21.0;
pub const COMPOSER_CONTROL_RIGHT: f32 = 21.0;
pub const COMPOSER_CONTROL_BOTTOM: f32 = 22.0;
pub const COMPOSER_PLUS_WIDTH: f32 = 58.0;
pub const COMPOSER_ATTACH_WIDTH: f32 = 92.0;
pub const COMPOSER_IMAGE_WIDTH: f32 = 90.0;
pub const COMPOSER_CODE_WIDTH: f32 = 88.0;
pub const COMPOSER_TERMINAL_WIDTH: f32 = 106.0;
pub const COMPOSER_TOOLS_WIDTH: f32 = 110.0;
pub const COMPOSER_SEND_WIDTH: f32 = 54.0;
pub const COMPOSER_MODEL_MIN_WIDTH: f32 = 178.0;
pub const METADATA_GAP: f32 = glass::TOOL_GAP;
// Charge the actual controls, outer padding and every fixed gap. These are
// unscaled widths; dividing the available width by font_scale is conservative
// for the unscaled Plus/Send/gaps and exact at the reference UI size.
pub const COMPOSER_TERMINAL_MIN_AVAILABLE: f32 = COMPOSER_CONTROL_LEFT
    + COMPOSER_CONTROL_RIGHT
    + COMPOSER_PLUS_WIDTH
    + COMPOSER_TERMINAL_WIDTH
    + COMPOSER_TOOLS_WIDTH
    + COMPOSER_SEND_WIDTH
    + 3.0 * METADATA_GAP;
pub const COMPOSER_MODEL_MIN_AVAILABLE: f32 =
    COMPOSER_TERMINAL_MIN_AVAILABLE + COMPOSER_MODEL_MIN_WIDTH + METADATA_GAP;
pub const COMPOSER_SECONDARY_MIN_WIDTH: f32 = COMPOSER_MODEL_MIN_AVAILABLE
    + COMPOSER_ATTACH_WIDTH
    + COMPOSER_IMAGE_WIDTH
    + COMPOSER_CODE_WIDTH
    + 3.0 * METADATA_GAP;
pub const MODE_CONTROL_HEIGHT: f32 = 44.0;
pub const MODE_CONTROL_MIN_WIDTH: f32 = 90.0;
pub const SEARCH_HEIGHT: f32 = 44.0;
pub const SEARCH_WIDTH: f32 = 542.0;
pub const SEARCH_CENTER_OFFSET: f32 = 0.0;
pub const HEADER_ACTION_SIZE: f32 = 44.0;
pub const HEADER_ACTION_GAP: f32 = 8.0;
pub const PROFILE_DIAMETER: f32 = 50.0;
pub const INSPECTOR_TAB_WIDTH: f32 = 82.0;
pub const NAVIGATION_LABEL_TOP: f32 = 84.0;
pub const WORKSPACE_SELECTOR_TOP: f32 = 107.0;
pub const WORKSPACE_SELECTOR_HEIGHT: f32 = 38.0;
pub const NAVIGATION_ITEMS_TOP: f32 = 153.0;
pub const NAVIGATION_ROW_HEIGHT: f32 = 38.0;
pub const NAVIGATION_ROW_GAP: f32 = 6.0;
pub const RECENT_ROW_HEIGHT: f32 = 42.0;
pub const SPACES_ROW_HEIGHT: f32 = 46.0;
pub const NAVIGATION_STATUS_HEIGHT: f32 = 60.0;
pub const INSPECTOR_METADATA_ROW_HEIGHT: f32 = 28.0;
pub const INSPECTOR_CURRENT_HEIGHT: f32 = 59.0;
pub const INSPECTOR_QUEUE_ROW_HEIGHT: f32 = 32.0;
pub const INSPECTOR_OUTPUT_HEIGHT: f32 = 170.0;

const fn rgb(c: [u8; 4]) -> [u8; 3] {
    [c[0], c[1], c[2]]
}

pub mod color {
    use super::{glass, rgb};
    pub const TITLEBAR: [u8; 3] = rgb(glass::TOP);
    pub const RAIL: [u8; 3] = rgb(glass::RAIL);
    pub const NAVIGATION: [u8; 3] = rgb(glass::NAVIGATION);
    pub const NAVIGATION_SELECTED: [u8; 3] = rgb(glass::SELECTED_NAV);
    pub const CANVAS: [u8; 3] = rgb(glass::CENTER);
    pub const INSPECTOR: [u8; 3] = rgb(glass::INSPECTOR);
    pub const INSPECTOR_CARD: [u8; 3] = rgb(glass::INSPECTOR_CARD);
    pub const COMPOSER: [u8; 3] = rgb(glass::COMPOSER);
    pub const CONTROL_SURFACE: [u8; 3] = rgb(glass::BUTTON);
    pub const ACCENT_PALE: [u8; 3] = rgb(glass::BUTTON_HOVER);
    pub const TERMINAL: [u8; 3] = rgb(glass::TERMINAL);
    pub const DARK_BORDER: [u8; 3] = rgb(glass::BORDER);
    pub const LIGHT_BORDER: [u8; 3] = rgb(glass::BORDER);
    pub const CONTROL_BORDER: [u8; 3] = rgb(glass::BORDER);
    pub const PROFILE_BORDER: [u8; 3] = rgb(glass::BORDER);
    pub const PROFILE_BACKGROUND: [u8; 3] = rgb(glass::BUTTON);
    pub const CHROME_TEXT: [u8; 3] = rgb(glass::TEXT);
    pub const CANVAS_TEXT: [u8; 3] = rgb(glass::TEXT);
    pub const MESSAGE_USER_TEXT: [u8; 3] = rgb(glass::TEXT);
    pub const CHROME_MUTED: [u8; 3] = rgb(glass::MUTED_TEXT);
    pub const CANVAS_MUTED: [u8; 3] = rgb(glass::MUTED_TEXT);
    pub const MESSAGE_BODY: [u8; 3] = rgb(glass::MUTED_TEXT);
    pub const MESSAGE_STEP_TEXT: [u8; 3] = rgb(glass::MUTED_TEXT);
    pub const DISABLED_TEXT: [u8; 3] = rgb(glass::DISABLED_TEXT);
    pub const ACCENT: [u8; 3] = rgb(glass::ACCENT_TOP);
    pub const USER_BUBBLE: [u8; 3] = rgb(glass::SELECTED_NAV);
    pub const DIFF_ADDED_BACKGROUND: [u8; 3] = [18, 59, 47];
    pub const DIFF_REMOVED_BACKGROUND: [u8; 3] = [74, 28, 46];
    pub const DIFF_ADDED_TEXT: [u8; 3] = [139, 231, 182];
    pub const DIFF_REMOVED_TEXT: [u8; 3] = [255, 165, 180];
}

/// Straight background RGBA only. Foreground role values remain alpha255.
pub fn rgba(value: [u8; 3], opacity: f32) -> iced::Color {
    let rgba = if value == color::TITLEBAR {
        glass::TOP
    } else if value == color::RAIL {
        glass::RAIL
    } else if value == color::NAVIGATION {
        glass::NAVIGATION
    } else if value == color::NAVIGATION_SELECTED {
        glass::SELECTED_NAV
    } else if value == color::CANVAS {
        glass::CENTER
    } else if value == color::INSPECTOR {
        glass::INSPECTOR
    } else if value == color::INSPECTOR_CARD {
        glass::INSPECTOR_CARD
    } else if value == color::COMPOSER {
        glass::COMPOSER
    } else if value == color::CONTROL_SURFACE {
        glass::BUTTON
    } else if value == color::ACCENT_PALE {
        glass::BUTTON_HOVER
    } else if value == color::TERMINAL {
        glass::TERMINAL
    } else if value == color::DARK_BORDER {
        glass::BORDER
    } else if value == color::DIFF_ADDED_BACKGROUND {
        [18, 59, 47, 210]
    } else if value == color::DIFF_REMOVED_BACKGROUND {
        [74, 28, 46, 210]
    } else {
        [value[0], value[1], value[2], 255]
    };
    let alpha = if rgba[3] < 255 {
        rgba[3] as f32 / 255.0 * opacity.clamp(0.05, 1.0)
    } else {
        1.0
    };
    iced::Color::from_rgba8(rgba[0], rgba[1], rgba[2], alpha)
}
