//! Suggested sessions: the commands behind the sidebar's light bulb (see
//! `code_assistant_core::suggestions`). The snapshot itself arrives as
//! `UiEvent::SuggestionsChanged` and is kept in `Gpui::suggestions`.

use code_assistant_core::suggestions::{Dismissal, SuggestionsSnapshot};
use code_assistant_core::ui::UiEvent;
use tracing::debug;

use super::super::Gpui;

impl Gpui {
    /// The suggestions as last published.
    pub(crate) fn suggestions_snapshot(&self) -> SuggestionsSnapshot {
        self.suggestions.lock().unwrap().clone()
    }

    /// Take the core's current suggestions, e.g. at startup or after the
    /// event stream lagged.
    pub(crate) fn cmd_load_suggestions(&self) {
        let Some(service) = self.session_service() else {
            return;
        };
        // The first read loads the state file: off the UI thread.
        let gpui = self.clone();
        self.dispatch(async move {
            gpui.push_event(UiEvent::SuggestionsChanged {
                snapshot: service.suggestions(),
            });
        });
    }

    pub(crate) fn cmd_refresh_suggestions(&self) {
        let Some(service) = self.session_service() else {
            return;
        };
        self.dispatch(async move {
            if let Err(e) = service.refresh_suggestions().await {
                debug!("Suggestion refresh failed: {e:#}");
            }
        });
    }

    pub(crate) fn cmd_mark_suggestions_seen(&self, ids: Vec<String>) {
        let Some(service) = self.session_service() else {
            return;
        };
        self.dispatch(async move {
            service.mark_suggestions_seen(&ids);
        });
    }

    pub(crate) fn cmd_dismiss_suggestion(&self, id: String, never: bool) {
        let Some(service) = self.session_service() else {
            return;
        };
        let how = if never {
            Dismissal::Never
        } else {
            Dismissal::NotNow
        };
        self.dispatch(async move {
            service.dismiss_suggestion(&id, how);
        });
    }

    /// Start the suggested session and show it, with the template's first
    /// message in the input (unless the template sent it).
    pub(crate) fn cmd_start_suggestion(&self, id: String) {
        let Some(service) = self.session_service() else {
            return;
        };
        let gpui = self.clone();
        self.display_status("Starting the suggested session…");
        self.dispatch(async move {
            match service.start_suggestion(id).await {
                Ok(started) => {
                    if !started.sent {
                        gpui.save_draft_for_session(
                            &started.session_id,
                            &started.first_message,
                            &[],
                            None,
                        );
                    }
                    *gpui.current_session_id.lock().unwrap() = Some(started.session_id.clone());
                    gpui.cmd_refresh_chat_list();
                    gpui.cmd_load_session(started.session_id, None);
                    if !started.notices.is_empty() {
                        gpui.display_status(started.notices.join(" · "));
                    }
                }
                Err(e) => gpui.push_event(UiEvent::DisplayError {
                    message: format!("Cannot start the suggested session: {e:#}"),
                }),
            }
        });
    }
}
