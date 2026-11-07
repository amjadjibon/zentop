//! Desktop version and upstream links. Opening releases is user initiated.
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
                    .p_6()
                    .gap_5()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(img(APP_ICON.clone()).size_10())
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(concat!("Zentop v", env!("CARGO_PKG_VERSION"))),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(
                                                "by Amjad Hossain · based on NeoHtop by Abdenasser",
                                            ),
                                    ),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .bg(cx.theme().secondary)
                                    .text_xs()
                                    .child("Open source"),
                            ),
                    )
                    .child(
                        h_flex()
                            .justify_center()
                            .p_3()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(img(APP_ICON.clone()).size(px(80.))),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .text_xs()
                            .child(div().text_color(cx.theme().muted_foreground).child("Stack"))
                            .child("Rust · GPUI Kit"),
                    )
                    .child(
                        Button::new("source")
                            .link()
                            .small()
                            .label("Source on GitHub ↗")
                            .on_click(|_, _, cx| cx.open_url(REPOSITORY)),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .pt_3()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Desktop version information"),
                            )
                            .child(
                                Button::new("updates")
                                    .link()
                                    .small()
                                    .label("Check for updates…")
                                    .on_click(|_, _, cx| {
                                        cx.open_url(&format!("{REPOSITORY}/releases/latest"))
                                    }),
                            ),
                    ),
            )
    }
}
