//! Compose the picker's rows.
//!
//! The geometry, input line, rules, footer, empty state, preview column and
//! scrollbar derive from herdr-bar (MIT, see LICENSE.herdr-bar). Everything is drawn as full rows into the popup's inner
//! area: Herdr already draws the popup border, so the picker stays borderless
//! and full-bleed. Rows are composed as (role, text) segments, measured in
//! display columns, and only then turned into escape sequences -- which keeps
//! truncation honest for CJK and emoji.

use crate::fuzzy::Prepared;
use crate::icons;
use crate::pystr;
use crate::textutil::{display_width, pad, truncate, truncate_middle, window_positions};
use crate::theme::{Theme, BOLD, RESET, UNDERLINE};
use std::collections::BTreeMap;

pub const PROMPT: &str = "❯";
pub const SELECTED_BAR: &str = "▌";
pub const SCROLL_TRACK: &str = "│";
pub const SCROLL_THUMB: &str = "┃";
pub const SEPARATOR: &str = " │ ";

pub fn status_word(status: &str) -> &'static str {
    match status {
        "blocked" => "needs you",
        "working" => "working",
        "done" => "done",
        "idle" => "idle",
        _ => "",
    }
}

pub const DETAIL_SEP: &str = " — ";
pub const MIN_DETAIL: isize = 6;
pub const MIN_TITLE: isize = 16;
pub const META_SHARE: f64 = 0.45;
pub const MIN_LIST: isize = 46;
pub const PREVIEW_MIN: isize = 28;
pub const PREVIEW_MAX: isize = 56;

/// One metadata chunk on the right of a row. Chunks are dropped leftmost
/// first when the row runs out of room, so the most important goes last.
#[derive(Clone, Debug)]
pub struct Meta {
    pub role: &'static str,
    pub text: String,
    /// Search field whose matches light this chunk up.
    pub field: Option<usize>,
    /// OSC 8 target; the chunk is also a click target.
    pub link: Option<String>,
}

impl Meta {
    pub fn new(role: &'static str, text: impl Into<String>) -> Meta {
        Meta { role, text: text.into(), field: None, link: None }
    }
}

/// A list row before layout: what it says, and what search looks at.
#[derive(Clone, Debug)]
pub struct Item {
    pub title: String,
    pub detail: String,
    /// Search field index of `detail`, or -1.
    pub detail_field: isize,
    /// Lifecycle status for the glyph: blocked | working | done | idle | none;
    /// empty draws no glyph at all.
    pub status: String,
    /// Agent kind whose logo leads the row, when icons are on.
    pub agent: Option<String>,
    pub meta: Vec<Meta>,
    /// Tree depth: worktrees 0, their space 1, its tabs 2.
    pub indent: usize,
    /// Glyph (role, text) after the indent: which level of the tree this is.
    pub icon: Option<(&'static str, &'static str)>,
    /// Search fields, prepared: field 0 is the title; the others are searched
    /// but drawn only where the detail names them.
    pub prepared: Vec<Prepared>,
}

impl Item {
    pub fn new(title: String, detail: String, status: String, agent: Option<String>, meta: Vec<Meta>, mut fields: Vec<String>) -> Item {
        fields.insert(0, title.clone());
        let detail_field = if detail.is_empty() { -1 } else { fields.iter().position(|f| *f == detail).map(|i| i as isize).unwrap_or(-1) };
        let prepared = fields.iter().map(|f| Prepared::new(f)).collect();
        Item { title, detail, detail_field, status, agent, meta, indent: 0, icon: None, prepared }
    }

    pub fn tree(mut self, indent: usize, icon: Option<(&'static str, &'static str)>) -> Item {
        self.indent = indent;
        self.icon = icon;
        self
    }
}

/// An item plus where the query landed in it.
#[derive(Clone, Debug)]
pub struct Row {
    pub item: usize,
    pub positions: Vec<usize>,
    pub field_positions: BTreeMap<usize, Vec<usize>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub list_top: isize,
    pub list_height: isize,
    pub rules: bool,
    pub footer: bool,
    pub inner_width: isize,
    pub list_width: isize,
    pub preview_width: isize,
}

/// `header` rows (a pixel wordmark) sit above the input line and push the list down.
pub fn compute_layout(width: isize, height: isize, preview: bool, header: isize) -> Layout {
    let rules = height >= 8;
    let footer = height >= 6;
    let list_top = header + if rules { 2 } else { 1 };
    let chrome = list_top + if rules && footer { 2 } else if footer { 1 } else { 0 };
    let list_height = (height - chrome).max(1);
    let inner = (width - 2).max(1);
    let mut preview_width = 0;
    let mut list_width = inner;
    let separator = pystr::len(SEPARATOR) as isize;
    if preview && inner >= PREVIEW_MIN + MIN_LIST + separator {
        preview_width = PREVIEW_MAX.min(PREVIEW_MIN.max(inner / 3));
        list_width = inner - preview_width - separator;
    }
    Layout { list_top, list_height, rules, footer, inner_width: inner, list_width, preview_width }
}

#[derive(Clone, Debug)]
pub struct Segment {
    pub role: &'static str,
    pub text: String,
    pub bold: bool,
    pub underline: bool,
    pub link: Option<String>,
}

impl Segment {
    pub fn new(role: &'static str, text: impl Into<String>) -> Segment {
        Segment { role, text: text.into(), bold: false, underline: false, link: None }
    }

    pub fn bold(role: &'static str, text: impl Into<String>, bold: bool) -> Segment {
        Segment { role, text: text.into(), bold, underline: false, link: None }
    }
}

fn link_safe(url: &str) -> String {
    url.chars().filter(|c| !c.is_control()).collect()
}

pub fn segments_to_text(theme: &Theme, segments: &[Segment], background: &str) -> String {
    let mut out = String::new();
    for segment in segments {
        if segment.text.is_empty() {
            continue;
        }
        if let Some(url) = &segment.link {
            out.push_str("\x1b]8;;");
            out.push_str(&link_safe(url));
            out.push_str("\x1b\\");
        }
        let color = if segment.role.is_empty() { "" } else { theme.fg(segment.role) };
        if color.is_empty() && !segment.bold && !segment.underline {
            out.push_str(&segment.text);
        } else {
            if segment.bold {
                out.push_str(BOLD);
            }
            if segment.underline {
                out.push_str(UNDERLINE);
            }
            out.push_str(color);
            out.push_str(&segment.text);
            out.push_str(RESET);
            out.push_str(background);
        }
        if segment.link.is_some() {
            out.push_str("\x1b]8;;\x1b\\");
        }
    }
    out
}

fn highlighted_label(theme: &Theme, text: &str) -> String {
    segments_to_text(theme, &[Segment::bold("accent", text, true)], "")
}

pub fn plain_width(segments: &[Segment]) -> isize {
    segments.iter().map(|s| display_width(&s.text) as isize).sum()
}

fn highlight(text: &str, positions: &[usize], base_role: &'static str, bold: bool) -> Vec<Segment> {
    if positions.is_empty() {
        return vec![Segment::bold(base_role, text, bold)];
    }
    let mut segments = Vec::new();
    let mut run = String::new();
    let mut run_marked = false;
    let flush = |segments: &mut Vec<Segment>, run: &str, marked: bool| {
        if run.is_empty() {
            return;
        }
        segments.push(Segment {
            role: if marked { "match" } else { base_role },
            text: run.to_string(),
            bold: bold || marked,
            underline: marked,
            link: None,
        });
    };
    for (index, c) in text.chars().enumerate() {
        let marked = positions.contains(&index);
        if marked != run_marked && !run.is_empty() {
            flush(&mut segments, &run, run_marked);
            run.clear();
        }
        run_marked = marked;
        run.push(c);
    }
    flush(&mut segments, &run, run_marked);
    segments
}

pub fn agent_role(agent: Option<&str>) -> &'static str {
    let Some(agent) = agent.filter(|a| !a.is_empty()) else { return "muted" };
    match icons::agent_kind(agent).as_str() {
        "claude" => "agent_claude",
        "codex" => "agent_codex",
        "kimi" => "agent_kimi",
        "pi" => "agent_pi",
        "grok" => "agent_grok",
        "gemini" => "agent_gemini",
        "cursor" | "cursor-agent" => "agent_cursor",
        "opencode" | "open-code" => "agent_opencode",
        _ => "muted",
    }
}

pub fn status_role(status: &str) -> &'static str {
    match status {
        "blocked" => "blocked",
        "working" => "working",
        "done" => "done",
        "idle" => "idle",
        _ => "unknown",
    }
}

fn meta_segments(meta: &Meta, row: &Row) -> Vec<Segment> {
    let marks: Vec<usize> = meta
        .field
        .and_then(|field| row.field_positions.get(&field))
        .map(|marks| marks.iter().copied().filter(|&m| m < pystr::len(&meta.text)).collect())
        .unwrap_or_default();
    let mut segments = highlight(&meta.text, &marks, meta.role, false);
    if let Some(url) = &meta.link {
        for segment in &mut segments {
            segment.link = Some(url.clone());
        }
    }
    segments
}

/// A composed row, and the columns (relative to its start) of its link, if any.
pub struct RenderedRow {
    pub text: String,
    pub link: Option<(isize, isize, String)>,
}

pub fn render_row(theme: &Theme, item: &Item, row: &Row, width: isize, selected: bool) -> RenderedRow {
    let background = if selected { theme.selection_bg.as_str() } else { "" };
    let marker =
        if selected { Segment::new("accent", format!("{} ", SELECTED_BAR)) } else { Segment::new("", "  ") };
    let glyph_role = status_role(&item.status);
    let glyph = (!item.status.is_empty())
        .then(|| Segment::new(glyph_role, format!("{} ", crate::theme::status_glyph(&item.status))));
    let logo = if theme.agent_icons { icons::logo_for(item.agent.as_deref()) } else { None };
    let mut leading = vec![marker];
    if item.indent > 0 {
        leading.push(Segment::new("unknown", "  ".repeat(item.indent)));
    }
    if let Some((role, icon)) = item.icon {
        leading.push(Segment::new(role, format!("{} ", icon)));
    }
    if let Some(logo) = logo {
        leading.push(Segment::new(agent_role(item.agent.as_deref()), format!("{}  ", logo)));
    }
    leading.extend(glyph);

    let mut meta: Vec<Vec<Segment>> = item.meta.iter().map(|m| meta_segments(m, row)).collect();
    let fixed = plain_width(&leading);
    let available = (width - fixed).max(0);
    let meta_budget = MIN_TITLE.max((available as f64 * META_SHARE) as isize);
    while !meta.is_empty() && (meta_width(&meta) > meta_budget || available - meta_width(&meta) - 2 < MIN_TITLE) {
        meta.remove(0);
    }
    let meta_w = if meta.is_empty() { 0 } else { meta_width(&meta) };
    let title_width = (available - if meta_w > 0 { meta_w + 2 } else { 0 }).max(1);

    let (title_text, positions, _) = window_positions(&item.title, title_width, &row.positions);
    let title_role = if item.status == "working" { "working" } else { "text" };
    let mut title_segments = highlight(&title_text, &positions, title_role, selected);

    let mut used = plain_width(&title_segments);
    if !item.detail.is_empty() {
        let room = title_width - used - display_width(DETAIL_SEP) as isize;
        if room >= MIN_DETAIL {
            let empty = Vec::new();
            let detail_marks = if item.detail_field >= 0 {
                row.field_positions.get(&(item.detail_field as usize)).unwrap_or(&empty)
            } else {
                &empty
            };
            let (detail_text, marks, _) = window_positions(&item.detail, room, detail_marks);
            title_segments.push(Segment::new("unknown", DETAIL_SEP));
            title_segments.extend(highlight(&detail_text, &marks, "muted", false));
            used = plain_width(&title_segments);
        }
    }
    let gap = if meta_w > 0 { (available - used - meta_w).max(1) } else { available - used };
    let mut segments = leading;
    segments.extend(title_segments);
    segments.push(Segment::new("", " ".repeat(gap.max(0) as usize)));
    if meta_w > 0 {
        segments.extend(join_meta(meta));
    }

    let mut link = None;
    let mut x = 0;
    for segment in &segments {
        let w = display_width(&segment.text) as isize;
        if let Some(url) = &segment.link {
            link = Some(match link {
                Some((start, _, url)) => (start, x + w, url),
                None => (x, x + w, url.clone()),
            });
        }
        x += w;
    }

    let body = segments_to_text(theme, &segments, background);
    let trailing = (width - plain_width(&segments)).max(0) as usize;
    let text = if !background.is_empty() {
        format!("{}{}{}{}", background, body, " ".repeat(trailing), RESET)
    } else {
        format!("{}{}", body, " ".repeat(trailing))
    };
    RenderedRow { text, link }
}

fn join_meta(meta: Vec<Vec<Segment>>) -> Vec<Segment> {
    let mut out = Vec::new();
    for (index, chunk) in meta.into_iter().enumerate() {
        if index > 0 {
            out.push(Segment::new("unknown", " · "));
        }
        out.extend(chunk);
    }
    out
}

fn meta_width(meta: &[Vec<Segment>]) -> isize {
    if meta.is_empty() {
        return 0;
    }
    meta.iter().map(|chunk| plain_width(chunk)).sum::<isize>() + 3 * (meta.len() as isize - 1)
}

fn slice(text: &str, start: usize, stop: usize) -> String {
    text.chars().skip(start).take(stop.saturating_sub(start)).collect()
}

pub fn render_input(
    theme: &Theme,
    query: &str,
    cursor: usize,
    width: isize,
    placeholder: &str,
    chip: &str,
    prompt_text: &str,
) -> String {
    let prompt = [Segment::bold("accent", format!("{} ", prompt_text), true)];
    input_after(theme, &prompt, query, cursor, width, placeholder, chip)
}

/// The wordmark the main search line carries after its prompt: "her" in the
/// text colour, "trees" in the leaf (idle) role, then a dim separator.
const WORDMARK_LABEL: [(&str, &str); 3] = [("text", "her"), ("idle", "trees"), ("unknown", " │ ")];

/// Narrower than this, the search line drops the wordmark so the placeholder
/// and the scope chip keep their room.
pub const WORDMARK_MIN_WIDTH: isize = 60;

/// The main picker's query line: `❯ hertrees │ query…` when `label` is set and
/// the line is wide enough, the plain prompt otherwise. The query, its cursor
/// and the placeholder start after the separator.
#[allow(clippy::too_many_arguments)]
pub fn render_search(
    theme: &Theme,
    query: &str,
    cursor: usize,
    width: isize,
    placeholder: &str,
    short_placeholder: &str,
    chip: &str,
    label: bool,
) -> String {
    let mut prompt = vec![Segment::bold("accent", format!("{} ", PROMPT), true)];
    if label && width >= WORDMARK_MIN_WIDTH {
        prompt.extend(WORDMARK_LABEL.iter().map(|(role, text)| Segment::bold(role, *text, *role != "unknown")));
    }
    // The full placeholder needs 62 columns counting the bare prompt.
    let label_width = plain_width(&prompt) - plain_width(&prompt[..1]);
    let placeholder = if width - label_width >= 62 { placeholder } else { short_placeholder };
    input_after(theme, &prompt, query, cursor, width, placeholder, chip)
}

fn input_after(
    theme: &Theme,
    prompt: &[Segment],
    query: &str,
    cursor: usize,
    width: isize,
    placeholder: &str,
    chip: &str,
) -> String {
    let mut chip_text = String::new();
    let mut chip_width = 0;
    let prompt_width = plain_width(prompt);
    if !chip.is_empty() && width > display_width(chip) as isize + prompt_width + 24 {
        chip_text = format!(" {} ", chip);
        chip_width = display_width(&chip_text) as isize;
    }
    let field_width = (width - prompt_width - chip_width).max(1);
    let prompt = segments_to_text(theme, prompt, "");

    let mut rendered;
    let used;
    if !query.is_empty() {
        let visible = truncate(query, field_width);
        let length = pystr::len(&visible);
        let cursor_index = cursor.min(length);
        let before = slice(&visible, 0, cursor_index);
        let under = if cursor_index < length { slice(&visible, cursor_index, cursor_index + 1) } else { " ".into() };
        let after = if cursor_index < length { slice(&visible, cursor_index + 1, length) } else { String::new() };
        rendered = format!(
            "{}{}\x1b[7m{}\x1b[27m{}",
            prompt,
            segments_to_text(theme, &[Segment::new("text", before.clone())], ""),
            under,
            segments_to_text(theme, &[Segment::new("text", after.clone())], "")
        );
        used = prompt_width
            + display_width(&before) as isize
            + display_width(&under) as isize
            + display_width(&after) as isize;
    } else {
        let hint = truncate(placeholder, (field_width - 2).max(0));
        rendered =
            format!("{}\x1b[7m \x1b[27m{}", prompt, segments_to_text(theme, &[Segment::new("unknown", hint.clone())], ""));
        used = prompt_width + 1 + display_width(&hint) as isize;
    }
    if !chip_text.is_empty() {
        let padding = (width - used - display_width(&chip_text) as isize).max(1);
        rendered.push_str(&" ".repeat(padding as usize));
        rendered.push_str(&highlighted_label(theme, &chip_text));
    }
    rendered
}

pub fn render_rule(theme: &Theme, width: isize) -> String {
    segments_to_text(theme, &[Segment::new("unknown", "─".repeat(width.max(0) as usize))], "")
}

pub fn render_footer(theme: &Theme, width: isize, hints: &[(&str, &str)], counter: &str) -> String {
    let mut segments: Vec<Segment> = Vec::new();
    for (index, (key, label)) in hints.iter().enumerate() {
        if index > 0 {
            segments.push(Segment::new("unknown", "  "));
        }
        segments.push(Segment::new("muted", *key));
        if !label.is_empty() {
            segments.push(Segment::new("unknown", format!(" {}", label)));
        }
    }
    let mut used = plain_width(&segments);
    let counter_width = display_width(counter) as isize;
    while !segments.is_empty() && used + counter_width + 2 > width {
        segments.pop();
        while let Some(last) = segments.last() {
            if last.role == "unknown" && pystr::strip(&last.text).is_empty() {
                segments.pop();
            } else {
                break;
            }
        }
        used = plain_width(&segments);
    }
    let padding = (width - used - counter_width).max(1);
    segments.push(Segment::new("", " ".repeat(padding as usize)));
    segments.push(Segment::new("unknown", counter));
    segments_to_text(theme, &segments, "")
}

/// A one-line message in the footer's place: a flash, progress, or an error.
pub fn render_status(theme: &Theme, width: isize, role: &'static str, text: &str, keys: &str) -> String {
    let keys_width = display_width(keys) as isize;
    let room = if keys.is_empty() { width } else { width - keys_width - 2 };
    let mut segments = vec![Segment::bold(role, truncate(text, room.max(1)), role == "blocked")];
    if !keys.is_empty() && room > 8 {
        let padding = (width - plain_width(&segments) - keys_width).max(1);
        segments.push(Segment::new("", " ".repeat(padding as usize)));
        segments.push(Segment::new("muted", keys));
    }
    segments_to_text(theme, &segments, "")
}

/// An armed action: the affected item on the left, answering keys on the right.
pub fn render_confirm(theme: &Theme, width: isize, action: &str, title: &str, keys: &str) -> String {
    let room = (width - display_width(keys) as isize - display_width(action) as isize - 6).max(6);
    let mut segments = vec![
        Segment::bold("blocked", format!("{} ", action), true),
        Segment::bold("text", format!("“{}”", truncate(title, room)), true),
        Segment::new("blocked", "?"),
    ];
    let mut used = plain_width(&segments);
    let keys = if used + display_width(keys) as isize + 1 > width { "⏎/esc" } else { keys };
    if used + display_width(keys) as isize + 1 > width {
        segments = vec![Segment::bold("blocked", truncate(&format!("{} “{}”?", action, title),
            (width - display_width(keys) as isize - 1).max(0)), true)];
        used = plain_width(&segments);
    }
    let padding = (width - used - display_width(keys) as isize).max(1);
    if used + padding + display_width(keys) as isize > width {
        return segments_to_text(theme, &[Segment::bold("blocked", truncate(&format!("{}?", action), width), true)], "");
    }
    segments.push(Segment::new("", " ".repeat(padding as usize)));
    segments.push(Segment::new("muted", keys));
    segments_to_text(theme, &segments, "")
}

/// The brand mark in block characters (shepherd, lantern, tree).
pub const MARK: &str = include_str!("../branding/terminal/mark.txt");

/// The rows of block art, without blank rows above or below it.
pub fn mark_rows(art: &str, blank: impl Fn(&str) -> bool) -> Vec<&str> {
    let rows: Vec<&str> = art.lines().collect();
    let start = rows.iter().position(|row| !blank(row)).unwrap_or(rows.len());
    let end = rows.iter().rposition(|row| !blank(row)).map_or(start, |last| last + 1);
    rows[start..end].to_vec()
}

/// The plain mark, centered in `width`, or nothing when it does not fit: the
/// shepherd in the accent colour, the lantern amber, the tree green, the
/// ground muted. Theme roles, so it follows the configured colours.
pub fn render_mark(theme: &Theme, width: isize) -> Vec<String> {
    let rows = mark_rows(MARK, |row| row.trim().is_empty());
    let mark_width = rows.iter().map(|row| display_width(row)).max().unwrap_or(0) as isize;
    if mark_width > width {
        return Vec::new();
    }
    let left = " ".repeat(((width - mark_width) / 2) as usize);
    let last = rows.len().saturating_sub(1);
    rows.iter().enumerate().map(|(index, row)| {
        let mut segments: Vec<Segment> = Vec::new();
        for (column, ch) in row.chars().enumerate() {
            let role = match (ch, column) {
                (' ', _) => "",
                _ if index == last => "muted",
                (_, 0..=12) => "accent",
                (_, 13..=19) => "working",
                _ => "idle",
            };
            match segments.last_mut() {
                Some(segment) if segment.role == role => segment.text.push(ch),
                _ => segments.push(Segment::new(role, ch.to_string())),
            }
        }
        pad(&format!("{}{}", left, segments_to_text(theme, &segments, "")), width)
    }).collect()
}

pub fn render_empty_state(theme: &Theme, width: isize, lines: &[(&str, &'static str, bool)], filter: &str) -> Vec<String> {
    let mut out = Vec::new();
    for &(text, role, bold) in lines {
        if text.is_empty() {
            continue;
        }
        let mut text = text.to_string();
        let rendered = if !filter.is_empty() && text == format!("nothing in {} filter", filter) && display_width(&text) as isize <= width {
            format!(
                "{}{}{}",
                segments_to_text(theme, &[Segment::bold(role, "nothing in ", bold)], ""),
                highlighted_label(theme, filter),
                segments_to_text(theme, &[Segment::bold(role, " filter", bold)], "")
            )
        } else {
            text = truncate(&text, width);
            segments_to_text(theme, &[Segment::bold(role, text.clone(), bold)], "")
        };
        let left = if width - (display_width(&text) as isize) < 0 { 0 } else { (width - display_width(&text) as isize) / 2 };
        out.push(pad(&format!("{}{}", " ".repeat(left as usize), rendered), width));
    }
    out
}

/// The right column: a bold title (— detail), a muted path, a rule, then
/// pre-styled body lines from the top. Lines are cut, never wrapped.
pub fn render_preview(
    theme: &Theme,
    title: &str,
    detail: &str,
    subtitle: &str,
    body: &[Vec<Segment>],
    width: isize,
    height: isize,
) -> Vec<String> {
    if height <= 0 || width <= 0 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut header = vec![Segment::bold("text", truncate(title, width), true)];
    let room = width - plain_width(&header) - display_width(DETAIL_SEP) as isize;
    if !detail.is_empty() && room >= MIN_DETAIL {
        header.push(Segment::new("unknown", DETAIL_SEP));
        header.push(Segment::new("muted", truncate(detail, room)));
    }
    out.push(segments_to_text(theme, &header, ""));
    out.push(if !subtitle.is_empty() {
        segments_to_text(theme, &[Segment::new("muted", truncate_middle(subtitle, width))], "")
    } else {
        String::new()
    });
    out.push(segments_to_text(theme, &[Segment::new("unknown", "─".repeat(width as usize))], ""));
    for line in body {
        if out.len() as isize >= height {
            break;
        }
        out.push(segments_to_text(theme, &clip(line, width), ""));
    }
    out.truncate(height as usize);
    out
}

/// Cut styled segments to a display width, ellipsing the segment that overflows.
pub fn clip(segments: &[Segment], width: isize) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut used = 0;
    for segment in segments {
        let w = display_width(&segment.text) as isize;
        if used + w <= width {
            out.push(segment.clone());
            used += w;
            continue;
        }
        let mut cut = segment.clone();
        cut.text = truncate(&segment.text, width - used);
        out.push(cut);
        break;
    }
    out
}

/// Row index -> scrollbar glyph for the visible window.
pub fn scrollbar_column(total: isize, visible: isize, offset: isize, height: isize) -> BTreeMap<isize, &'static str> {
    let mut marks = BTreeMap::new();
    if total <= visible || height <= 0 {
        return marks;
    }
    let thumb = 1.max(((height * visible) as f64 / total as f64).round_ties_even() as isize);
    let span = 1.max(total - visible);
    let start = ((height - thumb) as f64 * (offset as f64 / span as f64)).round_ties_even() as isize;
    for row in 0..height {
        marks.insert(row, if start <= row && row < start + thumb { SCROLL_THUMB } else { SCROLL_TRACK });
    }
    marks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::textutil::{strip_ansi, visible_width};

    fn item(title: &str, meta: Vec<Meta>) -> Item {
        Item::new(title.into(), ".worktrees/x".into(), "none".into(), None, meta, vec![".worktrees/x".into(), "#259".into()])
    }

    fn row() -> Row {
        Row { item: 0, positions: Vec::new(), field_positions: BTreeMap::new() }
    }

    #[test]
    fn mark_drops_blank_rows_and_fits_or_is_left_out() {
        assert_eq!(mark_rows("\n  \nab\n\ncd\n \n", |r| r.trim().is_empty()), ["ab", "", "cd"]);
        assert!(mark_rows(" \n", |r| r.trim().is_empty()).is_empty());
        let theme = Theme::new(None, None, None);
        let mark = render_mark(&theme, 40);
        assert_eq!(mark.len(), mark_rows(MARK, |r| r.trim().is_empty()).len());
        assert!(mark.iter().all(|row| visible_width(row) == 40));
        assert!(!strip_ansi(&mark[0]).trim().is_empty());
        assert!(render_mark(&theme, 31).is_empty());
    }

    const WIDE: &str = "search worktrees, branches, PRs…    @ agents   ⇥ PR state";
    const SHORT: &str = "search worktrees, branches, PRs…";

    #[test]
    fn search_line_carries_the_two_tone_wordmark_when_wide() {
        let theme = Theme::new(None, None, None);
        let line = render_search(&theme, "", 0, 100, WIDE, SHORT, "everything", true);
        let plain = strip_ansi(&line);
        assert!(plain.starts_with("❯ hertrees │  search worktrees, branches, PRs…    @ agents"), "{plain:?}");
        assert!(plain.ends_with(" everything "));
        assert_eq!(visible_width(&line), 100);
        // "her" in the text role, "trees" in the leaf (idle) role, a dim separator.
        assert!(line.contains(&format!("{}her{}", theme.fg("text"), RESET)));
        assert!(line.contains(&format!("{}trees{}", theme.fg("idle"), RESET)));
        assert!(line.contains(&format!("{} │ {}", theme.fg("unknown"), RESET)));
        // Between the full placeholder's width and the label's, the short one.
        let mid = strip_ansi(&render_search(&theme, "", 0, 66, WIDE, SHORT, "everything", true));
        assert!(mid.starts_with("❯ hertrees │  search worktrees, branches, PRs…  "), "{mid:?}");
        assert!(!mid.contains("@ agents") && mid.ends_with(" everything "));
    }

    #[test]
    fn search_line_drops_the_wordmark_when_narrow() {
        let theme = Theme::new(None, None, None);
        let narrow = render_search(&theme, "", 0, WORDMARK_MIN_WIDTH - 1, WIDE, SHORT, "everything", true);
        assert_eq!(strip_ansi(&narrow), strip_ansi(&render_input(&theme, "", 0, WORDMARK_MIN_WIDTH - 1, SHORT, "everything", PROMPT)));
        assert!(!strip_ansi(&narrow).contains("hertrees"));
        for width in [WORDMARK_MIN_WIDTH, WORDMARK_MIN_WIDTH + 1] {
            let text = strip_ansi(&render_search(&theme, "", 0, width, WIDE, SHORT, "everything", true));
            assert!(text.starts_with("❯ hertrees │ "), "{text:?}");
            assert!(text.contains("search worktrees") && text.ends_with(" everything "), "{text:?}");
            assert_eq!(display_width(&text) as isize, width);
        }
        for width in [1, 10, 30, 59, 60, 61, 72, 73, 140] {
            assert!(visible_width(&render_search(&theme, "feat 界", 3, width, WIDE, SHORT, "✓ merged", true)) as isize <= width.max(14));
        }
    }

    #[test]
    fn search_line_without_its_label_is_the_plain_prompt() {
        let theme = Theme::new(None, None, None);
        let plain = render_search(&theme, "", 0, 100, WIDE, SHORT, "everything", false);
        assert_eq!(strip_ansi(&plain), strip_ansi(&render_input(&theme, "", 0, 100, WIDE, "everything", PROMPT)));
        assert!(!strip_ansi(&plain).contains("hertrees"));
    }

    #[test]
    fn search_cursor_sits_in_the_query_after_the_separator() {
        let theme = Theme::new(None, None, None);
        let line = render_search(&theme, "feat", 2, 80, WIDE, SHORT, "", true);
        let (before, after) = line.split_once("\x1b[7m").expect("an inverse cursor");
        assert_eq!(strip_ansi(before), "❯ hertrees │ fe");
        assert!(after.starts_with("a\x1b[27m"));
        assert_eq!(strip_ansi(&line), "❯ hertrees │ feat");
        // At the end, the cursor is a blank cell after the query.
        let end = render_search(&theme, "feat", 4, 80, WIDE, SHORT, "", true);
        assert_eq!(strip_ansi(end.split_once("\x1b[7m").unwrap().0), "❯ hertrees │ feat");
        // Narrow: the query follows the bare prompt.
        let narrow = render_search(&theme, "feat", 0, 40, WIDE, SHORT, "", true);
        assert_eq!(strip_ansi(narrow.split_once("\x1b[7m").unwrap().0), "❯ ");
    }

    #[test]
    fn confirmation_fits_narrow_and_wide_footers() {
        let theme = Theme::new(None, None, None);
        for width in [1, 5, 12, 24, 60, 100] {
            let text = render_confirm(&theme, width, "delete", "long 界 preset title", "⏎ delete · esc keep");
            assert!(visible_width(&text) as isize <= width);
        }
        assert!(strip_ansi(&render_confirm(&theme, 100, "delete", "plan", "⏎ delete · esc keep")).contains("delete “plan”?"));
    }

    #[test]
    fn link_columns_follow_the_chip_and_rows_fill_the_width() {
        let theme = Theme::new(None, None, None);
        let mut chip = Meta::new("accent", "◉ open #259");
        chip.link = Some("https://github.com/o/r/pull/259".into());
        let it = item("feat/hero-badge-redesign", vec![Meta::new("muted", "2h04m"), chip]);
        // MIN_LIST up: narrower, the title's floor squeezes every chunk out.
        for width in [MIN_LIST, 60, 100] {
            let rendered = render_row(&theme, &it, &row(), width, false);
            assert_eq!(visible_width(&rendered.text) as isize, width);
            let plain: Vec<char> = strip_ansi(&rendered.text).chars().collect();
            let (start, end, url) = rendered.link.expect("chip survives: it is the last meta chunk");
            assert_eq!(url, "https://github.com/o/r/pull/259");
            let label: String = plain[start as usize..end as usize].iter().collect();
            assert_eq!(label, "◉ open #259");
            assert_eq!(end, width, "the chip is right-aligned");
        }
        assert!(render_row(&theme, &it, &row(), 100, false).text.contains("\x1b]8;;https://github.com/o/r/pull/259\x1b\\"));
    }

    #[test]
    fn link_url_cannot_inject_escapes() {
        let theme = Theme::new(None, None, None);
        let mut chip = Meta::new("accent", "#1");
        chip.link = Some("https://x/\x1b]52;c;evil\x07".into());
        let text = render_row(&theme, &item("b", vec![chip]), &row(), 60, false).text;
        assert!(!text.contains("\x07"));
        assert_eq!(text.matches('\x1b').count(), text.matches("\x1b]8;;").count() + text.matches("\x1b\\").count() + text.matches("\x1b[").count());
    }
}
