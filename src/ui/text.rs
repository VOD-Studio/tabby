use ratatui::text::Span;
use unicode_width::UnicodeWidthChar;

use crate::{
    api::error::ApiError,
    core::{Action, CoreError, Step},
};

/// One-line, localized summary of a failed request, for toasts and error pages.
pub fn describe(err: &ApiError) -> String {
    match err {
        ApiError::Unauthorized => "Secret 错误，请用 --secret 或修改配置".to_owned(),
        ApiError::NotFound => "接口不存在（404）".to_owned(),
        ApiError::Http { status, .. } => format!("控制器返回错误（HTTP {status}）"),
        ApiError::Timeout => "请求超时".to_owned(),
        ApiError::Connect(_) => "无法连接控制器".to_owned(),
        ApiError::Decode(_) => "控制器返回了无法解析的数据".to_owned(),
    }
}

/// Why something the user asked the kernel to do failed: the kernel's own
/// message when it refused (it says more than the status, e.g. that there
/// is no newer version to upgrade to), else [`describe`].
pub fn reason(err: &ApiError) -> String {
    match err {
        ApiError::Http { message, .. } if !message.is_empty() => message.clone(),
        _ => describe(err),
    }
}

/// The untranslated cause behind [`describe`], when it adds information.
pub fn detail(err: &ApiError) -> Option<&str> {
    match err {
        ApiError::Connect(cause) | ApiError::Decode(cause) => Some(cause),
        ApiError::Http { message, .. } if !message.is_empty() => Some(message),
        ApiError::Http { .. } | ApiError::Unauthorized | ApiError::NotFound | ApiError::Timeout => {
            None
        }
    }
}

/// The headline of the page for a managed kernel that could not be brought
/// up.
pub fn core_title(err: &CoreError) -> String {
    match err {
        CoreError::NoBinary { path: Some(path) } => format!("找不到内核 {}", path.display()),
        CoreError::NoBinary { path: None } => "没有找到 mihomo".to_owned(),
        CoreError::PortTaken(port) => format!("端口 {port} 已被其他程序占用"),
        CoreError::StartFailed { .. } => "内核启动失败".to_owned(),
        CoreError::Command { action, .. } => match action {
            Action::Start => "无法启动内核",
            Action::Restart => "无法重启内核",
            Action::Stop => "无法停止内核",
        }
        .to_owned(),
        CoreError::Io { .. } => "内核的文件无法读写".to_owned(),
    }
}

/// What goes under [`core_title`]: what the kernel said, or what to do.
pub fn core_detail(err: &CoreError) -> Vec<String> {
    let lines = |text: &str| text.lines().map(str::to_owned).collect();
    match err {
        CoreError::NoBinary { .. } => {
            vec!["请安装 mihomo，或在 config.toml 的 [core] 里用 binary 指定路径".to_owned()]
        }
        CoreError::PortTaken(_) => vec!["控制器端口被别的程序占用，托管内核用不了它".to_owned()],
        CoreError::StartFailed { log } => lines(log),
        CoreError::Command { message, .. } => lines(message),
        CoreError::Io { context, source } => vec![format!("{context}: {source}")],
    }
}

/// One line for a toast: what the managed kernel said, else the headline.
pub fn core_reason(err: &CoreError) -> String {
    core_detail(err)
        .into_iter()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_else(|| core_title(err))
}

/// What the screen waiting for the managed kernel says it is doing.
pub fn core_step(step: Step) -> &'static str {
    match step {
        Step::Starting => "正在启动内核…",
        Step::WaitingController => "等待控制器…",
    }
}

/// Breaks `spans` into lines at most `width` columns wide, each piece keeping
/// its style. Wide characters are never split; a space a line would start
/// with is dropped.
pub fn wrap(spans: Vec<Span<'static>>, width: usize) -> Vec<Vec<Span<'static>>> {
    let mut lines: Vec<Vec<Span<'static>>> = vec![vec![]];
    let mut used = 0;
    for span in spans {
        let mut piece = String::new();
        for ch in span.content.chars() {
            let ch_width = ch.width().unwrap_or(0);
            if used > 0 && used + ch_width > width {
                if !piece.is_empty() {
                    lines
                        .last_mut()
                        .unwrap_or_else(|| unreachable!())
                        .push(Span::styled(std::mem::take(&mut piece), span.style));
                }
                lines.push(vec![]);
                used = 0;
            }
            if used == 0 && ch == ' ' && lines.len() > 1 {
                continue;
            }
            piece.push(ch);
            used += ch_width;
        }
        if !piece.is_empty() {
            lines
                .last_mut()
                .unwrap_or_else(|| unreachable!())
                .push(Span::styled(piece, span.style));
        }
    }
    lines
}

/// Cuts `spans` to at most `width` columns, ending in `…` in the style of
/// the span it cut into when anything was cut. Wide characters are never
/// split.
pub fn truncate_spans(spans: Vec<Span<'_>>, width: usize) -> Vec<Span<'_>> {
    if spans.iter().map(Span::width).sum::<usize>() <= width {
        return spans;
    }
    if width == 0 {
        return vec![];
    }
    // One column is kept for the ellipsis.
    let budget = width - 1;
    let mut used = 0;
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let span_width = span.width();
        if used + span_width <= budget {
            used += span_width;
            out.push(span);
            continue;
        }
        let mut piece = String::new();
        for ch in span.content.chars() {
            let ch_width = ch.width().unwrap_or(0);
            if used + ch_width > budget {
                break;
            }
            used += ch_width;
            piece.push(ch);
        }
        piece.push('…');
        out.push(Span::styled(piece, span.style));
        break;
    }
    out
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Style};

    use super::*;

    fn texts(lines: &[Vec<Span<'static>>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.iter().map(|span| span.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn wrapping_keeps_wide_characters_whole_and_styles_apart() {
        let muted = Style::new().fg(Color::DarkGray);
        let lines = wrap(vec![Span::raw("abc"), Span::styled("本 de", muted)], 4);
        // `本` would straddle the edge of the first line: it moves down whole.
        assert_eq!(texts(&lines), ["abc", "本 d", "e"]);
        assert_eq!(lines[1][0].style, muted);
        // A line does not start with the space it broke at.
        assert_eq!(texts(&wrap(vec![Span::raw("ab cd")], 2)), ["ab", "cd"]);
    }

    #[test]
    fn truncated_spans_end_in_an_ellipsis_in_the_style_they_cut() {
        let muted = Style::new().fg(Color::DarkGray);
        let spans = || vec![Span::raw(" 连接"), Span::styled("  12/30 ", muted)];
        // 13 columns: they fit as they are.
        assert_eq!(texts(&[truncate_spans(spans(), 13)]), [" 连接  12/30 "]);
        let cut = truncate_spans(spans(), 12);
        assert_eq!(texts(std::slice::from_ref(&cut)), [" 连接  12/3…"]);
        assert_eq!(cut[1].style, muted);
        // `接` would straddle the edge: it goes whole, the ellipsis takes
        // the first span's style.
        let cut = truncate_spans(spans(), 4);
        assert_eq!(texts(std::slice::from_ref(&cut)), [" 连…"]);
        assert_eq!(cut.len(), 1);
        assert_eq!(texts(&[truncate_spans(spans(), 3)]), [" …"]);
        assert_eq!(texts(&[truncate_spans(spans(), 0)]), [""]);
    }
}
