//! The DNS overrides of a managed core (plan §7.2): the keys of `[dns]` in
//! `settings.toml`, edited in a popup of the settings page.

use crossterm::event::Event;
use tui_input::{Input, backend::crossterm::EventHandler};

use super::{Action, App, Change, CoreEvent, Overlay, ToastKind, managed::failure};
use crate::{
    core::{CoreError, DNS_KEYS, DnsKind},
    ui::scroll::Scroll,
};

/// Cursor and the text being typed in the DNS popup.
#[derive(Debug, Default)]
pub struct DnsView {
    /// Index into [`DNS_KEYS`].
    pub cursor: usize,
    pub scroll: Scroll,
    /// What is being typed for the key under the cursor, while a text or a
    /// list is. It has the keyboard meanwhile, as a filter box does.
    pub editing: Option<Input>,
    /// The kernel is checking a change.
    pub checking: bool,
    /// Something was changed: the kernel reads its config again when the
    /// popup closes.
    changed: bool,
}

/// What the popup calls `key`.
pub fn label(key: &str) -> &'static str {
    match key {
        "listen" => "监听地址",
        "ipv6" => "IPv6 解析",
        "prefer-h3" => "优先 HTTP/3",
        "enhanced-mode" => "增强模式",
        "fake-ip-filter" => "FakeIP 过滤",
        "fake-ip-filter-mode" => "过滤模式",
        "default-nameserver" => "引导 DNS",
        "nameserver" => "主 DNS",
        "fallback" => "备用 DNS",
        "proxy-server-nameserver" => "代理节点 DNS",
        "direct-nameserver" => "直连 DNS",
        _ => "",
    }
}

/// `value` as it is typed and shown: a list is its items, comma apart.
pub fn show(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => text.clone(),
        toml::Value::Boolean(true) => "开".to_owned(),
        toml::Value::Boolean(false) => "关".to_owned(),
        toml::Value::Array(items) => items
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

/// What typing `text` for a key of `kind` sets: nothing typed lets the
/// subscription's value stand.
fn typed(kind: DnsKind, text: &str) -> Option<toml::Value> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(match kind {
        DnsKind::List => toml::Value::Array(
            text.split(|ch: char| matches!(ch, ',' | '，') || ch.is_whitespace())
                .filter(|item| !item.is_empty())
                .map(|item| toml::Value::String(item.to_owned()))
                .collect(),
        ),
        _ => toml::Value::String(text.to_owned()),
    })
}

/// What `Enter` on a switch or a choice sets next: the subscription's
/// value, then each way round, then the subscription's again.
fn next(kind: DnsKind, current: Option<&toml::Value>) -> Option<toml::Value> {
    match kind {
        DnsKind::Switch => match current {
            None => Some(toml::Value::Boolean(true)),
            Some(toml::Value::Boolean(true)) => Some(toml::Value::Boolean(false)),
            Some(_) => None,
        },
        DnsKind::Choice(words) => {
            let word = match current {
                None => words.first(),
                Some(value) => {
                    let at = words
                        .iter()
                        .position(|word| Some(*word) == value.as_str())?;
                    words.get(at + 1)
                }
            };
            word.map(|word| toml::Value::String((*word).to_owned()))
        }
        DnsKind::Text | DnsKind::List => None,
    }
}

impl App {
    /// `Enter` on `DNS 覆盖`: opens the popup.
    pub(super) fn open_dns(&mut self) {
        self.dns_view = DnsView::default();
        self.overlay = Some(Overlay::Dns);
        self.dirty = true;
    }

    /// The `[dns]` keys the managed core sets over its subscription's.
    pub fn dns_overrides(&self) -> std::collections::BTreeMap<String, toml::Value> {
        self.managed_core()
            .map(|core| core.settings().dns)
            .unwrap_or_default()
    }

    /// The key under the popup's cursor.
    fn dns_key(&self) -> &'static str {
        DNS_KEYS[self.dns_view.cursor.min(DNS_KEYS.len() - 1)]
    }

    /// The popup answers to the cursor keys, `Enter` and `Esc`; quitting and
    /// resizing are handled before it is asked. Typing a value, every key is
    /// text.
    pub(super) fn pick_dns(&mut self, action: &Action) {
        self.dirty = true;
        if let Some(input) = &mut self.dns_view.editing {
            match action {
                Action::FilterInput(key) => {
                    input.handle_event(&Event::Key(*key));
                }
                Action::FilterAccept => self.submit_dns_text(),
                Action::FilterClear => self.dns_view.editing = None,
                _ => {}
            }
            return;
        }
        match action {
            Action::Cursor(movement) => {
                let len = DNS_KEYS.len();
                let half = (len / 2).max(1);
                self.dns_view.cursor = movement.apply(self.dns_view.cursor, len, half);
            }
            Action::Confirm => self.activate_dns(),
            Action::Dismiss => self.close_overlay(),
            _ => {}
        }
    }

    /// `Enter` on a key: a switch or a choice goes to its next value, a text
    /// or a list opens for typing.
    fn activate_dns(&mut self) {
        if self.dns_view.checking {
            return;
        }
        let key = self.dns_key();
        let kind = DnsKind::of(key);
        let current = self.dns_overrides().remove(key);
        if matches!(kind, DnsKind::Text | DnsKind::List) {
            let text = current.as_ref().map(show).unwrap_or_default();
            self.dns_view.editing = Some(Input::new(text));
            return;
        }
        self.set_dns(key, next(kind, current.as_ref()));
    }

    /// `Enter` in the box: sets what was typed.
    fn submit_dns_text(&mut self) {
        let Some(input) = self.dns_view.editing.take() else {
            return;
        };
        let key = self.dns_key();
        let value = typed(DnsKind::of(key), input.value());
        if value != self.dns_overrides().remove(key) {
            self.set_dns(key, value);
        }
    }

    /// Has the kernel check the config with `value` for `key`, and keep it
    /// when it takes it.
    fn set_dns(&mut self, key: &'static str, value: Option<toml::Value>) {
        self.dns_view.checking = true;
        self.spawn_core(move |core, _| async move {
            CoreEvent::Dns {
                key,
                result: core.set_dns(key, value).await,
            }
        });
    }

    /// The kernel's verdict on a change.
    pub(super) fn on_dns(&mut self, key: &str, result: Result<(), CoreError>) {
        self.dirty = true;
        self.dns_view.checking = false;
        match result {
            // The popup was closed while the kernel was still looking.
            Ok(()) if self.overlay != Some(Overlay::Dns) => self.send_change(Change::Reload),
            Ok(()) => self.dns_view.changed = true,
            Err(CoreError::Invalid(why)) => {
                self.toast(ToastKind::Error, format!("内核不接受 {key}：{why}"));
            }
            Err(err) => self.toast(ToastKind::Error, failure("DNS 设置失败", &err)),
        }
    }

    /// The popup closed: the kernel reads the config again if anything was
    /// changed in it.
    pub(super) fn close_dns(&mut self) {
        self.dns_view.editing = None;
        if std::mem::take(&mut self.dns_view.changed) {
            self.send_change(Change::Reload);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_is_typed_with_commas_or_spaces_and_nothing_typed_is_the_subscriptions_own() {
        let list = |text| typed(DnsKind::List, text);
        assert_eq!(
            list("1.1.1.1, https://dns.google/dns-query  tls://8.8.8.8，system"),
            Some(toml::Value::Array(
                [
                    "1.1.1.1",
                    "https://dns.google/dns-query",
                    "tls://8.8.8.8",
                    "system"
                ]
                .map(|item| toml::Value::String(item.to_owned()))
                .to_vec()
            ))
        );
        assert_eq!(list("  "), None);
        assert_eq!(list(" , ,"), Some(toml::Value::Array(vec![])));
        assert_eq!(
            typed(DnsKind::Text, " 0.0.0.0:53 "),
            Some(toml::Value::String("0.0.0.0:53".to_owned()))
        );
    }

    #[test]
    fn a_switch_and_a_choice_go_round_and_end_on_the_subscriptions_own() {
        let word = |text: &str| toml::Value::String(text.to_owned());
        let modes = DnsKind::of("enhanced-mode");
        assert_eq!(next(modes, None), Some(word("fake-ip")));
        assert_eq!(
            next(modes, Some(&word("fake-ip"))),
            Some(word("redir-host"))
        );
        assert_eq!(next(modes, Some(&word("redir-host"))), None);
        // A word typed by hand into the file starts the round again at its
        // end: the subscription's own.
        assert_eq!(next(modes, Some(&word("normal"))), None);

        let switch = DnsKind::of("ipv6");
        assert_eq!(next(switch, None), Some(toml::Value::Boolean(true)));
        assert_eq!(
            next(switch, Some(&toml::Value::Boolean(true))),
            Some(toml::Value::Boolean(false))
        );
        assert_eq!(next(switch, Some(&toml::Value::Boolean(false))), None);
    }
}
