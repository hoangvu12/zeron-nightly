//! Settings → General → "Claude cloud sessions" (docs/design/provider-run-
//! chats.md §6 "Settings"): turn provider-run chats on for this device, and
//! pick which of its Claude logins act for them. The device's own login is
//! on by default; more accounts come later.
//!
//! The setting is this device's (it creates the sessions), so the card
//! always talks to the local engine. A change re-reads every composer's
//! agent catalog: "Anthropic cloud" appears in, or leaves, the place chip.

use gpui::{AnyElement, Context, Entity, Subscription, Task, div, prelude::*, px};
use zeron_engine::claude_cloud_sessions::{ClaudeCloudAccount, ClaudeCloudStatus};
use zeron_rpc::methods;

use crate::popover::Loadable;
use crate::settings::search::{SearchAnchor, SettingId};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

pub struct ClaudeCloudSessionsCard {
    state: Entity<AppState>,
    status: Loadable<ClaudeCloudStatus>,
    saving: bool,
    error: Option<String>,
    task: Option<Task<()>>,
    _state: Subscription,
}

/// What a switch on the card changes.
#[derive(Clone, Copy)]
enum Change {
    Enabled(bool),
    DeviceLogin(bool),
}

impl ClaudeCloudSessionsCard {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        // Settings can open before the engine connects: load once it does.
        let state_sub = cx.observe(&state, |this: &mut Self, _, cx| {
            if matches!(this.status, Loadable::Idle) {
                this.call(None, cx);
            }
        });
        let mut card = Self {
            state,
            status: Loadable::Idle,
            saving: false,
            error: None,
            task: None,
            _state: state_sub,
        };
        card.call(None, cx);
        card
    }

    /// `GetClaudeCloudSessions`, or `SetClaudeCloudSessions` for a change;
    /// either reply is the device's authoritative status.
    fn call(&mut self, change: Option<Change>, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let (method, params) = match change {
            Some(Change::Enabled(on)) => (
                methods::SET_CLAUDE_CLOUD_SESSIONS,
                serde_json::json!({ "enabled": on }),
            ),
            Some(Change::DeviceLogin(on)) => (
                methods::SET_CLAUDE_CLOUD_SESSIONS,
                serde_json::json!({ "deviceLogin": on }),
            ),
            None => (methods::GET_CLAUDE_CLOUD_SESSIONS, serde_json::json!({})),
        };
        if matches!(self.status, Loadable::Idle) {
            self.status = Loadable::Loading;
        }
        self.saving = change.is_some();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(method, params)
                .await
                .map_err(|error| match error {
                    zeron_rpc::RpcError::UnknownMethod(_) => {
                        "Update Zeron on this device to use Claude cloud sessions.".to_string()
                    }
                    _ => error.to_string(),
                })
                .and_then(|value| {
                    serde_json::from_value::<ClaudeCloudStatus>(value).map_err(|e| e.to_string())
                });
            this.update(cx, |card, cx| {
                let saved = card.saving;
                card.saving = false;
                match result {
                    Ok(status) => {
                        card.status = Loadable::Ready(status);
                        card.error = None;
                        if saved {
                            // The catalog gained or lost the provider-run
                            // harness: the place chip follows.
                            crate::pickers::bump_harness_catalog(cx);
                        }
                    }
                    Err(error) if matches!(card.status, Loadable::Ready(_)) => {
                        card.error = Some(error);
                    }
                    Err(error) => card.status = Loadable::Error(error),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn switch(
        &self,
        theme: &Theme,
        on: bool,
        key: &'static str,
        label: &'static str,
        interactive: bool,
        change: Change,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        widgets::toggle_switch(theme, on, key)
            .id(key)
            .flex_none()
            .role(gpui::Role::Switch)
            .aria_label(label)
            .aria_toggled(if on {
                gpui::Toggled::True
            } else {
                gpui::Toggled::False
            })
            .when(interactive, |el| {
                el.cursor_pointer()
                    .tab_index(0)
                    .focus_visible(|s| s.border_2().border_color(theme.accent))
                    .on_click(cx.listener(move |card, _, _, cx| card.call(Some(change), cx)))
            })
            .when(!interactive, |el| el.opacity(0.5))
            .into_any_element()
    }

    fn account_row(
        &self,
        theme: &Theme,
        account: &ClaudeCloudAccount,
        interactive: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = account
            .email
            .clone()
            .unwrap_or_else(|| "This device's Claude login".to_string());
        let meta = if account.email.is_some() {
            "This device's Claude login. Its cloud sessions start and show here."
        } else {
            "Not signed in: run `claude` on this device to sign in."
        };
        widgets::card_row(theme, false)
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.0))
                    .child(widgets::row_title(theme, title))
                    .child(widgets::meta_line(
                        theme,
                        vec![div().child(meta).into_any_element()],
                    )),
            )
            .child(self.switch(
                theme,
                account.enabled,
                "claude-cloud-device-login",
                "Use this device's Claude login",
                interactive,
                Change::DeviceLogin(!account.enabled),
                cx,
            ))
            .into_any_element()
    }
}

impl Render for ClaudeCloudSessionsCard {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::toast::take_error(&mut self.error, cx);
        let theme = Theme::of(cx).for_settings_surface();
        let status = self.status.ready().cloned();
        let enabled = status.as_ref().is_some_and(|s| s.settings.enabled || s.forced);
        let interactive = status
            .as_ref()
            .is_some_and(|s| s.available && !s.forced)
            && !self.saving;
        let meta = match (&self.status, status.as_ref()) {
            (_, Some(s)) if !s.available => "Cloud machines don't offer this.".to_string(),
            (_, Some(s)) if s.forced => "Set by ZERON_CLAUDE_CLOUD_SESSIONS".to_string(),
            (_, Some(_)) => {
                "Run chats in Anthropic's Claude Code cloud with your Claude login.".to_string()
            }
            (Loadable::Error(error), None) => error.clone(),
            _ => "Loading…".to_string(),
        };
        let mut card = widgets::section_card(&theme).child(
            widgets::card_row(&theme, true)
                .child(
                    div()
                        .flex_1()
                        .min_w(px(160.0))
                        .child(widgets::row_title(&theme, "Claude cloud sessions"))
                        .child(widgets::meta_line(
                            &theme,
                            vec![div().child(meta).into_any_element()],
                        )),
                )
                .child(self.switch(
                    &theme,
                    enabled,
                    "claude-cloud-sessions",
                    "Claude cloud sessions",
                    interactive,
                    Change::Enabled(!enabled),
                    cx,
                ))
                .search_anchor(SettingId::ClaudeCloudSessions),
        );
        // The accounts that act for it, once it's on.
        if let Some(status) = status.as_ref().filter(|_| enabled) {
            let accounts_interactive = status.available && !self.saving;
            for account in &status.accounts {
                card = card.child(self.account_row(&theme, account, accounts_interactive, cx));
            }
        }
        card
    }
}
