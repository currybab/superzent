use gpui::{Context, Keystroke, Render, Subscription, Window};
use ui::{Key, KeyIcon, PlatformStyle, render_modifiers};
use workspace::{StatusItemView, item::ItemHandle, ui::prelude::*};

pub struct PendingKeystrokeIndicator {
    pending_keystrokes: Option<Vec<Keystroke>>,
    _subscription: Subscription,
}

impl PendingKeystrokeIndicator {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription =
            cx.observe_pending_input(window, |this: &mut Self, window, _cx| {
                this.pending_keystrokes = window
                    .pending_input_keystrokes()
                    .map(|keystrokes| keystrokes.to_vec());
                _cx.notify();
            });

        Self {
            pending_keystrokes: None,
            _subscription: subscription,
        }
    }
}

fn render_keystroke(keystroke: &Keystroke) -> impl IntoElement {
    let platform_style = PlatformStyle::platform();
    let color = Some(Color::Muted);

    h_flex()
        .children(render_modifiers(
            &keystroke.modifiers,
            platform_style,
            color,
            None,
            false,
        ))
        .child(render_key_element(&keystroke.key, color, platform_style))
}

fn render_key_element(
    key: &str,
    color: Option<Color>,
    platform_style: PlatformStyle,
) -> AnyElement {
    match icon_for_key(key, platform_style) {
        Some(icon) => KeyIcon::new(icon, color).into_any_element(),
        None => {
            let mut key = key.to_string();
            if let Some(first) = key.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            Key::new(&key, color).into_any_element()
        }
    }
}

fn icon_for_key(key: &str, platform_style: PlatformStyle) -> Option<IconName> {
    match key {
        "left" => Some(IconName::ArrowLeft),
        "right" => Some(IconName::ArrowRight),
        "up" => Some(IconName::ArrowUp),
        "down" => Some(IconName::ArrowDown),
        "backspace" | "delete" => Some(IconName::Backspace),
        "return" | "enter" => Some(IconName::Return),
        "tab" => Some(IconName::Tab),
        "space" => Some(IconName::Space),
        "escape" => Some(IconName::Escape),
        "pagedown" => Some(IconName::PageDown),
        "pageup" => Some(IconName::PageUp),
        "shift" if platform_style == PlatformStyle::Mac => Some(IconName::Shift),
        "control" if platform_style == PlatformStyle::Mac => Some(IconName::Control),
        "platform" if platform_style == PlatformStyle::Mac => Some(IconName::Command),
        "alt" if platform_style == PlatformStyle::Mac => Some(IconName::Option),
        _ => None,
    }
}

impl Render for PendingKeystrokeIndicator {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(keystrokes) = self.pending_keystrokes.as_ref() else {
            return div().into_any_element();
        };

        h_flex()
            .gap(DynamicSpacing::Base04.rems(cx))
            .children(keystrokes.iter().map(|ks| render_keystroke(ks)))
            .child(
                Label::new("…")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .into_any()
    }
}

impl StatusItemView for PendingKeystrokeIndicator {
    fn set_active_pane_item(
        &mut self,
        _active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }
}
