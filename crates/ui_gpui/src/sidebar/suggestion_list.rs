//! The popover behind the sidebar's light bulb: sessions a template offers
//! for issues and pull requests (see `code_assistant_core::suggestions`).
//!
//! Rows are grouped by project. A click starts the session from its
//! template with the first message in the input; hovering a row offers
//! *Not now* (back when the issue or pull request changes) and *Never*.
//! What was new when the list opened keeps its dot until it closes.

use code_assistant_core::suggestions::{Suggestion, SuggestionsSnapshot};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    ClickEvent, Context, EventEmitter, Hsla, InteractiveElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px, rems,
};
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub enum SuggestionListEvent {
    /// Start the suggested session.
    Start { id: String },
    /// Put it away: until its subject changes, or for good.
    Dismiss { id: String, never: bool },
    /// Ask GitHub again.
    Refresh,
}

pub struct SuggestionList {
    snapshot: SuggestionsSnapshot,
    /// Unseen when the list opened; marked until it closes.
    fresh: HashSet<String>,
    hovered: Option<String>,
}

impl SuggestionList {
    pub fn new() -> Self {
        Self {
            snapshot: SuggestionsSnapshot::default(),
            fresh: HashSet::new(),
            hovered: None,
        }
    }

    pub fn set_snapshot(&mut self, snapshot: SuggestionsSnapshot, cx: &mut Context<Self>) {
        if self.snapshot != snapshot {
            self.snapshot = snapshot;
            cx.notify();
        }
    }

    /// The list opens: remember what is new now.
    pub fn opened(&mut self, cx: &mut Context<Self>) {
        self.fresh = self
            .snapshot
            .suggestions
            .iter()
            .filter(|s| !s.seen)
            .map(|s| s.id.clone())
            .collect();
        self.hovered = None;
        cx.notify();
    }

    fn render_row(&self, suggestion: &Suggestion, cx: &mut Context<Self>) -> impl IntoElement {
        let id = suggestion.id.clone();
        let hovered = self.hovered.as_deref() == Some(id.as_str());
        let fresh = self.fresh.contains(&id);
        let subject = format!(
            "#{} {}",
            suggestion.subject.number, suggestion.subject.title
        );
        let tooltip = SharedString::from(if suggestion.template_description.is_empty() {
            format!("{}\n{}", suggestion.subject.url, suggestion.reason)
        } else {
            format!(
                "{} — {}\n{}\n{}",
                suggestion.template,
                suggestion.template_description,
                suggestion.subject.url,
                suggestion.reason
            )
        });
        let start_id = id.clone();
        let hover_id = id.clone();
        div()
            .id(SharedString::from(format!("suggestion-{id}")))
            .debug_selector(|| "suggestion-row".to_string())
            .w_full()
            .px_2()
            .py(px(5.))
            .flex()
            .items_start()
            .gap_2()
            .rounded_sm()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().muted.opacity(0.4)))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let now = hovered.then(|| hover_id.clone());
                if this.hovered != now && (*hovered || this.hovered.as_ref() == Some(&hover_id)) {
                    this.hovered = now;
                    cx.notify();
                }
            }))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(SuggestionListEvent::Start {
                    id: start_id.clone(),
                })
            }))
            // The "new" dot, or the space it takes.
            .child(
                div()
                    .flex_none()
                    .mt(px(6.))
                    .size(px(6.))
                    .rounded_full()
                    .when(fresh, |el| el.bg(cx.theme().warning)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_sm()
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(cx.theme().foreground)
                                    .child(SharedString::from(suggestion.template.clone())),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_color(cx.theme().foreground.opacity(0.85))
                                    .child(SharedString::from(subject)),
                            ),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(format!(
                                "{} · {}",
                                suggestion.subject.repo.full_name(),
                                suggestion.reason
                            ))),
                    ),
            )
            .when(hovered, |el| {
                let not_now = id.clone();
                let never = id.clone();
                el.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .child(self.action(
                            format!("suggestion-later-{id}"),
                            "icons/alarm_clock.svg",
                            "Not now: back when it changes",
                            cx.theme().muted_foreground,
                            move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SuggestionListEvent::Dismiss {
                                    id: not_now.clone(),
                                    never: false,
                                })
                            },
                            cx,
                        ))
                        .child(self.action(
                            format!("suggestion-never-{id}"),
                            "icons/close.svg",
                            "Never for this",
                            cx.theme().muted_foreground,
                            move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SuggestionListEvent::Dismiss {
                                    id: never.clone(),
                                    never: true,
                                })
                            },
                            cx,
                        )),
                )
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn action(
        &self,
        id: String,
        icon: &'static str,
        tooltip: &'static str,
        color: Hsla,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(id))
            .flex_none()
            .size(px(20.))
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(move |s| s.bg(color.opacity(0.15)))
            .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
            .child(gpui_kit::svg().size(px(12.)).path(icon).text_color(color))
            .on_click(cx.listener(on_click))
    }
}

impl EventEmitter<SuggestionListEvent> for SuggestionList {}

impl Render for SuggestionList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Group by project, keeping the order suggestions arrive in.
        let mut groups: Vec<(String, Vec<Suggestion>)> = Vec::new();
        for suggestion in &self.snapshot.suggestions {
            match groups.iter_mut().find(|(p, _)| *p == suggestion.project) {
                Some((_, list)) => list.push(suggestion.clone()),
                None => groups.push((suggestion.project.clone(), vec![suggestion.clone()])),
            }
        }
        let mut rows: Vec<gpui_kit::AnyElement> = Vec::new();
        for (project, list) in &groups {
            rows.push(
                div()
                    .px_2()
                    .pt(px(6.))
                    .pb(px(2.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        gpui_kit::svg()
                            .flex_none()
                            .size(rems(0.7))
                            .path("icons/file_icons/folder.svg")
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(SharedString::from(project.clone()))
                    .into_any_element(),
            );
            for suggestion in list {
                rows.push(self.render_row(suggestion, cx).into_any_element());
            }
        }
        let refreshing = self.snapshot.refreshing;
        let max_height = px(f32::from(window.viewport_size().height) * 0.6);

        div()
            .id("suggestion-list")
            .w(px(340.))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py(px(6.))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(cx.theme().foreground)
                            .child("Suggested sessions"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .when(refreshing, |el| {
                                el.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Refreshing…"),
                                )
                            })
                            .when(!refreshing, |el| {
                                el.child(self.action(
                                    "suggestions-refresh".into(),
                                    "icons/rotate_cw.svg",
                                    "Ask GitHub again",
                                    cx.theme().muted_foreground,
                                    |_, _, _, cx| cx.emit(SuggestionListEvent::Refresh),
                                    cx,
                                ))
                            }),
                    ),
            )
            .child(
                div()
                    .id("suggestion-list-rows")
                    .w_full()
                    .max_h(max_height)
                    .p(px(4.))
                    .overflow_y_scrollbar()
                    .flex()
                    .flex_col()
                    .children(rows)
                    .when(groups.is_empty(), |el| {
                        el.child(
                            div()
                                .px_2()
                                .py_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Nothing suggested right now"),
                        )
                    }),
            )
            .when(!self.snapshot.problems.is_empty(), |el| {
                el.child(
                    div()
                        .flex_none()
                        .px_3()
                        .py(px(6.))
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().warning.opacity(0.08))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .children(
                            self.snapshot
                                .problems
                                .iter()
                                .take(4)
                                .map(|problem| div().child(SharedString::from(problem.clone()))),
                        ),
                )
            })
    }
}
