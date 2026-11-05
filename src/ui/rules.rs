use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Cell, Row, Table},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, RuleRow},
    format,
    ui::{
        COMPACT_WIDTH,
        components::{states, title},
        theme::Theme,
    },
};

/// `DOMAIN-KEYWORD`, the widest common type; longer ones are cut.
const TYPE_WIDTH: u16 = 14;

/// Columns between the cells.
const SPACING: u16 = 1;

const NO_VALUE: &str = "—";

/// Kernel spellings that [`type_name`]'s generic conversion gets wrong:
/// acronyms run together, and types a configuration spells another way.
const TYPE_NAMES: [(&str, &str); 10] = [
    ("GeoIP", "GEOIP"),
    ("SrcGeoIP", "SRC-GEOIP"),
    ("GeoSite", "GEOSITE"),
    ("IPCIDR", "IP-CIDR"),
    ("SrcIPCIDR", "SRC-IP-CIDR"),
    ("IPASN", "IP-ASN"),
    ("SrcIPASN", "SRC-IP-ASN"),
    ("IPSet", "IPSET"),
    ("Process", "PROCESS-NAME"),
    ("SubRules", "SUB-RULE"),
];

/// A rule type as configurations spell it: mihomo and Clash send
/// `DomainSuffix`, the table shows `DOMAIN-SUFFIX`. A type not known here is
/// split into words at its case changes (`FooBARBaz` → `FOO-BAR-BAZ`), so
/// every letter of the kernel's word stays; one already spelled that way is
/// left as it is.
pub fn type_name(kind: &str) -> String {
    if let Some((_, name)) = TYPE_NAMES.iter().find(|(raw, _)| *raw == kind) {
        return (*name).to_owned();
    }
    let chars: Vec<char> = kind.chars().collect();
    let mut name = String::with_capacity(kind.len() + 4);
    for (index, &ch) in chars.iter().enumerate() {
        if index > 0 && ch.is_uppercase() {
            let prev = chars[index - 1];
            let ends_acronym =
                prev.is_uppercase() && chars.get(index + 1).is_some_and(|next| next.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || ends_acronym {
                name.push('-');
            }
        }
        name.extend(ch.to_uppercase());
    }
    name
}

/// What a rule matches on, which picks the color of its type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleClass {
    /// `DOMAIN`, `DOMAIN-SUFFIX`, `DOMAIN-KEYWORD`, `DOMAIN-REGEX`, `GEOSITE` …
    Domain,
    /// `IP-CIDR` (mihomo reports `IP-CIDR6` rules as it too), `GEOIP`,
    /// `IP-ASN`, `SRC-IP-CIDR`, `IPSET` …
    Ip,
    /// `RULE-SET`, and `SUB-RULE`, which hands over to another list.
    Set,
    /// Processes, ports, networks, users, `AND` / `OR` / `NOT`, and types
    /// tabby does not know.
    Other,
    /// `MATCH`: everything that got this far.
    Match,
}

/// The class of a type named as [`type_name`] gives it.
pub fn class(name: &str) -> RuleClass {
    match name {
        "MATCH" => RuleClass::Match,
        "RULE-SET" | "SUB-RULE" => RuleClass::Set,
        "GEOSITE" => RuleClass::Domain,
        _ if name.split('-').next() == Some("DOMAIN") => RuleClass::Domain,
        _ if name
            .split('-')
            .any(|word| matches!(word, "IP" | "GEOIP" | "IPSET")) =>
        {
            RuleClass::Ip
        }
        _ => RuleClass::Other,
    }
}

fn class_color(class: RuleClass, theme: &Theme) -> Color {
    match class {
        RuleClass::Domain => theme.c2,
        RuleClass::Ip => theme.c3,
        RuleClass::Set => theme.c1,
        RuleClass::Other => theme.warning,
        RuleClass::Match => theme.success,
    }
}

/// Where traffic goes straight out reads as fine, where it is dropped as an
/// error; every proxy and group alike.
fn target_color(proxy: &str, theme: &Theme) -> Color {
    match proxy {
        "DIRECT" => theme.success,
        _ if proxy.starts_with("REJECT") => theme.error,
        _ => theme.fg,
    }
}

/// `text` lowercased character by character, as the filter compares it.
/// The same folding finds the matches [`highlight`] marks, so a row passes
/// the filter exactly when something in it lights up.
pub fn fold(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// For each character of `text`, whether it is part of a match of `needle`
/// (already folded). A character whose lowercase is longer than itself is
/// marked when any of it is.
fn marks(text: &str, needle: &str) -> Vec<bool> {
    let chars: Vec<char> = text.chars().collect();
    let mut marked = vec![false; chars.len()];
    if needle.is_empty() {
        return marked;
    }
    // The folded text, and for each of its bytes the character it came from.
    let mut folded = String::with_capacity(text.len());
    let mut owner = Vec::with_capacity(text.len());
    for (index, ch) in chars.iter().enumerate() {
        for lower in ch.to_lowercase() {
            folded.push(lower);
            owner.extend(std::iter::repeat_n(index, lower.len_utf8()));
        }
    }
    let mut from = 0;
    while let Some(found) = folded[from..].find(needle) {
        let start = from + found;
        let end = start + needle.len();
        for mark in &mut marked[owner[start]..=owner[end - 1]] {
            *mark = true;
        }
        from = end;
    }
    marked
}

/// `text` cut to `width` columns as [`format::truncate`] cuts it, in pieces
/// that alternate between outside and inside a match of `needle` (already
/// folded), whole characters each. The ellipsis of a cut text is never part
/// of a match.
pub fn highlight(text: &str, needle: &str, width: usize) -> Vec<(String, bool)> {
    let shown = format::truncate(text, width);
    let cut = shown != text;
    let kept = match (cut, width) {
        (false, _) => shown.chars().count(),
        (true, 0) => 0,
        (true, _) => shown.chars().count() - 1,
    };
    let mut pieces: Vec<(String, bool)> = vec![];
    for (ch, hit) in text.chars().zip(marks(text, needle)).take(kept) {
        match pieces.last_mut() {
            Some((piece, last)) if *last == hit => piece.push(ch),
            _ => pieces.push((ch.to_string(), hit)),
        }
    }
    if cut && width > 0 {
        pieces.push(("…".to_owned(), false));
    }
    pieces
}

fn panel_inner(area: Rect) -> Rect {
    Block::bordered().inner(area)
}

/// Table rows that fit in a page body `body`: the panel's inside less the
/// header.
pub fn capacity(body: Rect) -> usize {
    usize::from(panel_inner(body).height.saturating_sub(1))
}

/// The widths of the number, type, payload and target columns in a table
/// `inner` wide, for a list of `count` rules.
fn columns(inner: Rect, narrow: bool, count: usize) -> [usize; 4] {
    let digits = count.max(1).to_string().len();
    let number = if narrow {
        digits
    } else {
        digits.max(number_label(narrow).width())
    };
    let [number, kind, payload, proxy] = Layout::horizontal([
        Constraint::Length(u16::try_from(number).unwrap_or(u16::MAX)),
        Constraint::Length(TYPE_WIDTH),
        Constraint::Fill(3),
        Constraint::Fill(2),
    ])
    .spacing(SPACING)
    .areas(inner);
    [number, kind, payload, proxy].map(|rect| usize::from(rect.width))
}

fn number_label(narrow: bool) -> &'static str {
    if narrow { "#" } else { "序号" }
}

/// The rules page: one panel holding the table, in the kernel's order.
pub fn render(frame: &mut Frame, area: Rect, app: &App, rows: &[RuleRow]) {
    let theme = app.theme;
    let view = &app.rules_view;
    let visible = view.visible();
    let filter = view.filter.value();
    let needle = fold(filter);

    let count = if filter.is_empty() {
        format!("  {} ", rows.len())
    } else {
        format!("  {}/{} · 过滤「{filter}」 ", visible.len(), rows.len())
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent));
    let block = title::titled(
        block,
        area.width,
        vec![
            Span::styled(
                " 规则",
                Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(count, Style::new().fg(theme.muted)),
        ],
        vec![],
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if visible.is_empty() {
        if filter.is_empty() {
            states::empty(frame, inner, app, "没有规则", &[("r", "刷新")]);
        } else {
            states::empty(
                frame,
                inner,
                app,
                &format!("没有匹配「{filter}」的规则"),
                &[("Esc", "清除过滤"), ("/", "修改过滤")],
            );
        }
        return;
    }

    // Below this the number column drops its label for `#` and takes only
    // the width of the numbers.
    let narrow = app.size.width < COMPACT_WIDTH;
    let widths = columns(inner, narrow, rows.len());
    let cursor = view.cursor.min(visible.len() - 1);
    let window = view.scroll.window(cursor, visible.len(), capacity(area));

    let heading = Style::new().fg(theme.muted).add_modifier(Modifier::BOLD);
    let labels = [number_label(narrow), "类型", "匹配内容", "目标"];
    let header = Row::new(
        labels
            .iter()
            .zip(widths)
            .enumerate()
            .map(|(index, (label, width))| {
                let text = format::truncate(label, width);
                // The numbers are right-aligned; so is their label.
                let pad = if index == 0 {
                    " ".repeat(width.saturating_sub(text.width()))
                } else {
                    String::new()
                };
                Cell::from(Line::from(vec![
                    Span::raw(pad),
                    Span::styled(text, heading),
                ]))
            }),
    );
    // Only the rows in view are built: a kernel can have thousands of rules.
    let body = window.map(|index| {
        let style = if index == cursor {
            theme.surface
        } else if index % 2 == 1 {
            theme.stripe
        } else {
            Style::new()
        };
        Row::new(cells(&theme, visible[index], &needle, widths)).style(style)
    });
    let constraints = widths.map(|width| Constraint::Length(u16::try_from(width).unwrap_or(0)));
    frame.render_widget(
        Table::new(body, constraints)
            .header(header)
            .column_spacing(SPACING),
        inner,
    );
}

/// The cells of one rule; what matches `needle` stands out.
fn cells(theme: &Theme, row: &RuleRow, needle: &str, widths: [usize; 4]) -> [Cell<'static>; 4] {
    let [number_width, kind_width, payload_width, proxy_width] = widths;
    let hit = Style::new()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD | Modifier::REVERSED);
    let marked = |text: &str, width: usize, color: Color| {
        let spans: Vec<Span<'static>> = highlight(text, needle, width)
            .into_iter()
            .map(|(piece, matched)| {
                let style = if matched { hit } else { Style::new().fg(color) };
                Span::styled(piece, style)
            })
            .collect();
        Cell::from(Line::from(spans))
    };
    let number = format::truncate(&row.number.to_string(), number_width);
    let number = Cell::from(Span::styled(
        format!("{number:>number_width$}"),
        Style::new().fg(theme.muted),
    ));
    let kind = marked(&row.kind, kind_width, class_color(class(&row.kind), theme));
    let payload = if row.payload.is_empty() {
        Cell::from(Span::styled(NO_VALUE, Style::new().fg(theme.muted)))
    } else {
        marked(&row.payload, payload_width, theme.fg)
    };
    let proxy = marked(&row.proxy, proxy_width, target_color(&row.proxy, theme));
    [number, kind, payload, proxy]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_types_show_as_configurations_spell_them() {
        for (kind, name, class_of) in [
            ("DomainSuffix", "DOMAIN-SUFFIX", RuleClass::Domain),
            ("DomainKeyword", "DOMAIN-KEYWORD", RuleClass::Domain),
            ("Domain", "DOMAIN", RuleClass::Domain),
            ("GeoSite", "GEOSITE", RuleClass::Domain),
            ("IPCIDR", "IP-CIDR", RuleClass::Ip),
            ("SrcIPCIDR", "SRC-IP-CIDR", RuleClass::Ip),
            ("GeoIP", "GEOIP", RuleClass::Ip),
            ("IPASN", "IP-ASN", RuleClass::Ip),
            ("IPSuffix", "IP-SUFFIX", RuleClass::Ip),
            ("RuleSet", "RULE-SET", RuleClass::Set),
            // mihomo spells it out; original Clash does not.
            ("ProcessName", "PROCESS-NAME", RuleClass::Other),
            ("Process", "PROCESS-NAME", RuleClass::Other),
            ("ProcessPathRegex", "PROCESS-PATH-REGEX", RuleClass::Other),
            ("DstPort", "DST-PORT", RuleClass::Other),
            ("AND", "AND", RuleClass::Other),
            ("Match", "MATCH", RuleClass::Match),
            // Already spelled the configuration's way.
            ("DOMAIN-SUFFIX", "DOMAIN-SUFFIX", RuleClass::Domain),
            // Unknown: split at the case changes, nothing dropped.
            ("FooBARBaz2Qux", "FOO-BAR-BAZ2-QUX", RuleClass::Other),
        ] {
            assert_eq!(type_name(kind), name, "{kind}");
            assert_eq!(class(name), class_of, "{kind}");
        }
    }

    fn pieces(text: &str, needle: &str, width: usize) -> Vec<(String, bool)> {
        highlight(text, &fold(needle), width)
    }

    fn owned(pieces: &[(&str, bool)]) -> Vec<(String, bool)> {
        pieces
            .iter()
            .map(|(text, hit)| ((*text).to_owned(), *hit))
            .collect()
    }

    #[test]
    fn highlight_marks_every_match_whatever_the_case() {
        assert_eq!(
            pieces("Example.EXAMPLE.org", "exAMple", 40),
            owned(&[
                ("Example", true),
                (".", false),
                ("EXAMPLE", true),
                (".org", false)
            ])
        );
        assert_eq!(pieces("abc", "", 40), owned(&[("abc", false)]));
        assert_eq!(pieces("abc", "x", 40), owned(&[("abc", false)]));
    }

    #[test]
    fn highlight_keeps_wide_characters_whole() {
        assert_eq!(
            pieces("直连 A · 直连", "直连", 40),
            owned(&[("直连", true), (" A · ", false), ("直连", true)])
        );
        // `İ` lowercases to two characters; the match on its `i` marks it
        // whole, and the characters after it stay apart.
        assert_eq!(
            pieces("xİy", "i", 40),
            owned(&[("x", false), ("İ", true), ("y", false)])
        );
    }

    #[test]
    fn highlight_marks_what_is_left_of_a_cut_match() {
        // 7 columns: `节点` (4) and `选` would make 6, plus the ellipsis 7.
        assert_eq!(
            pieces("节点选择 A", "选择", 7),
            owned(&[("节点", false), ("选", true), ("…", false)])
        );
        // A match wholly past the cut lights nothing.
        assert_eq!(
            pieces("abcdef", "ef", 4),
            owned(&[("abc", false), ("…", false)])
        );
        // The pieces always add up to the truncated text.
        for width in 0..12 {
            let joined: String = pieces("ab直连cd直连", "直连", width)
                .into_iter()
                .map(|(piece, _)| piece)
                .collect();
            assert_eq!(
                joined,
                format::truncate("ab直连cd直连", width),
                "at {width}"
            );
        }
    }
}
