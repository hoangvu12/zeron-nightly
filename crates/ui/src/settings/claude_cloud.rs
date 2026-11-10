//! Settings → General → "Claude cloud sessions" (docs/design/provider-run-
//! chats.md §6 "Settings"): turn provider-run chats on for this device, and
//! pick which of its Claude logins act for them. The device's own login is
//! on by default; "Add account" signs another account in through the
//! official `claude` CLI (the person finishes in the browser). When the
//! device's login was switched, the card offers to add the previous
//! account.
//!
//! The setting is this device's (it creates the sessions), so the card
//! always talks to the local engine. A change re-reads every composer's
//! agent catalog: "Anthropic cloud" appears in, or leaves, the place chip.

use gpui::{AnyElement, Context, Entity, SharedString, Subscription, Task, div, prelude::*, px};
use zeron_engine::claude_cloud_sessions::{ClaudeCloudAccount, ClaudeCloudStatus, DEVICE_LOGIN};
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
    /// "Add account" is waiting on the CLI's sign-in.
    signing_in: bool,
    error: Option<String>,
    task: Option<Task<()>>,
    _state: Subscription,
}

/// What the card asks of the engine.
#[derive(Clone)]
enum Change {
    Enabled(bool),
    DeviceLogin(bool),
    /// An added account on or off (its id).
    Account(String, bool),
    /// "Add account": the CLI's sign-in, which may take minutes.
    Add,
    /// Decline the offer to add the device login's previous account.
    DismissOffer,
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
            signing_in: false,
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
        let (method, params) = match &change {
            Some(Change::Enabled(on)) => (
                methods::SET_CLAUDE_CLOUD_SESSIONS,
                serde_json::json!({ "enabled": on }),
            ),
            Some(Change::DeviceLogin(on)) => (
                methods::SET_CLAUDE_CLOUD_SESSIONS,
                serde_json::json!({ "deviceLogin": on }),
            ),
            Some(Change::Account(id, on)) => (
                methods::SET_CLAUDE_CLOUD_SESSIONS,
                serde_json::json!({ "account": {"id": id, "enabled": on} }),
            ),
            Some(Change::DismissOffer) => (
                methods::SET_CLAUDE_CLOUD_SESSIONS,
                serde_json::json!({ "dismissOffer": true }),
            ),
            Some(Change::Add) => (methods::ADD_CLAUDE_CLOUD_ACCOUNT, serde_json::json!({})),
            None => (methods::GET_CLAUDE_CLOUD_SESSIONS, serde_json::json!({})),
        };
        self.signing_in = matches!(change, Some(Change::Add));
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
                card.signing_in = false;
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
        (key, label): (SharedString, SharedString),
        interactive: bool,
        change: Change,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        widgets::toggle_switch(theme, on, key.clone())
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
                    .on_click(cx.listener(move |card, _, _, cx| {
                        card.call(Some(change.clone()), cx)
                    }))
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
        let device = account.id == DEVICE_LOGIN;
        let title = account.email.clone().unwrap_or_else(|| {
            if device {
                "This device's Claude login".to_string()
            } else {
                "Added Claude account".to_string()
            }
        });
        let meta = match (device, account.email.is_some()) {
            (true, true) => "This device's Claude login. Its cloud sessions start and show here.",
            (true, false) => "Not signed in: run `claude` on this device to sign in.",
            (false, true) => "Added on this device. Its cloud sessions show and run here.",
            (false, false) => "Signed out: add the account again to sign in.",
        };
        let (key, label, change) = if device {
            (
                SharedString::from("claude-cloud-device-login"),
                SharedString::from("Use this device's Claude login"),
                Change::DeviceLogin(!account.enabled),
            )
        } else {
            (
                SharedString::from(format!("claude-cloud-account-{}", account.id)),
                SharedString::from(format!("Use {title}")),
                Change::Account(account.id.clone(), !account.enabled),
            )
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
            .child(self.switch(theme, account.enabled, (key, label), interactive, change, cx))
            .into_any_element()
    }

    /// "Add account", or the offer to keep a switched-away account's chats.
    fn add_row(
        &self,
        theme: &Theme,
        offer: Option<&str>,
        interactive: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, meta) = match offer {
            Some(email) => (
                format!("Keep {email}'s cloud chats working on this device?"),
                "This device's Claude login changed. Sign in as that account again to add it."
                    .to_string(),
            ),
            None => (
                "Add account".to_string(),
                "Sign in to another Claude account with the official `claude` CLI, in your browser."
                    .to_string(),
            ),
        };
        let meta = if self.signing_in {
            "Waiting for the sign-in in your browser…".to_string()
        } else {
            meta
        };
        let add = widgets::text_action(
            theme,
            widgets::ActionTone::Filled,
            if offer.is_some() { "Add account" } else { "Add…" },
        )
        .id("claude-cloud-add-account")
        .role(gpui::Role::Button)
        .aria_label(match offer {
            Some(email) => format!("Add {email}"),
            None => "Add account".to_string(),
        })
        .when(interactive, |el| {
            el.on_click(cx.listener(|card, _, _, cx| card.call(Some(Change::Add), cx)))
        })
        .when(!interactive, |el| el.opacity(0.5));
        let mut actions = div().flex().flex_row().gap(px(6.0)).flex_none();
        if offer.is_some() {
            actions = actions.child(
                widgets::text_action(theme, widgets::ActionTone::Quiet, "Not now")
                    .id("claude-cloud-dismiss-offer")
                    .role(gpui::Role::Button)
                    .aria_label("Not now")
                    .when(interactive, |el| {
                        el.on_click(
                            cx.listener(|card, _, _, cx| card.call(Some(Change::DismissOffer), cx)),
                        )
                    }),
            );
        }
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
            .child(actions.child(add))
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
                "Start provider-run chats in Anthropic cloud with your Claude login.".to_string()
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
                    ("claude-cloud-sessions".into(), "Claude cloud sessions".into()),
                    interactive,
                    Change::Enabled(!enabled),
                    cx,
                ))
                .search_anchor(SettingId::ClaudeCloudSessions),
        );
        // The accounts that act for it, once it's on.
        if let Some(status) = status.as_ref().filter(|_| enabled) {
            let accounts_interactive = status.available && !self.saving;
            // A switched device login's prompt comes first, where it's seen.
            if let Some(offer) = status.offer.as_deref() {
                card = card.child(self.add_row(&theme, Some(offer), accounts_interactive, cx));
            }
            for account in &status.accounts {
                card = card.child(self.account_row(&theme, account, accounts_interactive, cx));
            }
            if status.offer.is_none() {
                card = card.child(self.add_row(&theme, None, accounts_interactive, cx));
            }
        }
        card
    }
}
