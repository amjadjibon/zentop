//! The About window.
use super::{APP_ICON, REPOSITORY};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Sizable, TitleBar};
use gpui_kit::{
    div, img, px, Context, FontWeight, IntoElement, ParentElement, Render, Styled, Window,
};

pub struct About;

impl Render for About {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new())
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .px_8()
                    .pb_8()
                    .child(img(APP_ICON.clone()).size(px(96.)))
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("NeoHtop"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(concat!("Version ", env!("CARGO_PKG_VERSION"))),
                    )
                    .child(
                        div()
                            .pt_2()
                            .text_sm()
                            .text_center()
                            .child("A modern, cross-platform system monitor."),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Built with Rust and GPUI Kit · MIT License"),
                    )
                    .child(
                        h_flex()
                            .pt_4()
                            .gap_2()
                            .child(
                                Button::new("updates")
                                    .primary()
                                    .small()
                                    .label("Check for Updates")
                                    .on_click(|_, _, cx| {
                                        cx.open_url(&format!("{REPOSITORY}/releases/latest"))
                                    }),
                            )
                            .child(
                                Button::new("source")
                                    .outline()
                                    .small()
                                    .label("Source Code")
                                    .on_click(|_, _, cx| cx.open_url(REPOSITORY)),
                            ),
                    ),
            )
    }
}
