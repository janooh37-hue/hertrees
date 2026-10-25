//! The configurable wordmark (`"wordmark"` in config.json).
//!
//! `search` puts the two-tone name on the search line (see
//! `render::render_search`); the pixel styles draw the brand lettering in the
//! popup's first rows, above a plain search line. The art below was generated
//! once from the brand grids (`branding/logo/wordmark/wordmark.svg`, the
//! icon's 16 px PNG and the compact 6 px / 4 px redrawings of the same letters)
//! and is kept as glyph rows, so nothing is parsed at run time.
//!
//! Each art row is `(glyphs, foreground roles, background roles)`, one role
//! letter per cell: `a` accent (the prompt's ❯ and the cloak), `h` text
//! ("her"), `t` idle (the leaf: "trees", the tree), `w` working (lantern
//! amber), `m` muted, `u` unknown, space for none. An empty background string
//! means no cell has one. Roles come from the theme, so colour overrides apply.

use crate::render::{segments_to_text, Segment};
use crate::textutil::{display_width, pad, truncate};
use crate::theme::{Theme, RESET};

pub type Art = &'static [(&'static str, &'static str, &'static str)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// `❯ hertrees │ search…` on the search line (the default).
    Search,
    /// 3-row half-block lettering.
    Pixel,
    /// 2-row half-block lettering.
    PixelSmall,
    /// The full 36×7 brand grid in 2 rows of braille.
    Braille,
    /// 2-row sextants (Unicode 13 block mosaics).
    Sextant,
    /// The 16×16 icon beside the name, tagline and counts.
    Icon,
    /// No name at all: a plain `❯ search…`.
    Off,
}

pub const NAMES: &[(&str, Style)] = &[
    ("search", Style::Search),
    ("pixel", Style::Pixel),
    ("pixel-small", Style::PixelSmall),
    ("braille", Style::Braille),
    ("sextant", Style::Sextant),
    ("icon", Style::Icon),
    ("off", Style::Off),
];

/// Rows the list keeps below a pixel wordmark; with fewer, the popup falls
/// back to the search-line wordmark.
pub const MIN_LIST_ROWS: isize = 5;

/// The icon style's text column starts this far right of the icon's left edge.
const ICON_TEXT: usize = 18;
const TAGLINE: [&str; 2] = ["every worktree, every agent,", "one popup."];

impl Style {
    /// A configured name; unknown names give None (the caller's default).
    pub fn parse(name: &str) -> Option<Style> {
        NAMES.iter().find(|(known, _)| *known == name).map(|(_, style)| *style)
    }

    fn art(self) -> Option<Art> {
        Some(match self {
            Style::Pixel => PIXEL,
            Style::PixelSmall => PIXEL_SMALL,
            Style::Braille => BRAILLE,
            Style::Sextant => SEXTANT,
            Style::Icon => ICON,
            Style::Search | Style::Off => return None,
        })
    }

    /// Rows drawn above the search line: the art, plus a blank row under the icon.
    pub fn rows(self) -> isize {
        match self.art() {
            Some(art) => art.len() as isize + (self == Style::Icon) as isize,
            None => 0,
        }
    }

    /// The narrowest inner width that holds the art (and the icon's text).
    pub fn min_width(self) -> isize {
        match self.art() {
            None => 0,
            Some(_) if self == Style::Icon => (ICON_TEXT + display_width(TAGLINE[0])) as isize,
            Some(art) => art_width(art),
        }
    }

    /// The style drawn in a popup of this inner width whose list would have
    /// `list_height` rows without any wordmark rows: a pixel style needs its
    /// width and must leave `MIN_LIST_ROWS` list rows, else it falls back to
    /// `Search` (which itself drops to the plain prompt below 60 columns).
    pub fn resolve(self, inner_width: isize, list_height: isize) -> Style {
        if self.art().is_none() {
            return self;
        }
        if inner_width >= self.min_width() && list_height - self.rows() >= MIN_LIST_ROWS {
            self
        } else {
            Style::Search
        }
    }
}

fn art_width(art: Art) -> isize {
    art.iter().map(|(glyphs, _, _)| display_width(glyphs)).max().unwrap_or(0) as isize
}

fn role(code: char) -> &'static str {
    match code {
        'a' => "accent",
        'h' => "text",
        't' => "idle",
        'w' => "working",
        'm' => "muted",
        'u' => "unknown",
        _ => "",
    }
}

/// One art row as escape sequences, runs of equal colours sharing a code.
fn paint_art_row(theme: &Theme, (glyphs, fg, bg): (&str, &str, &str)) -> String {
    let mut out = String::new();
    let mut fgs = fg.chars();
    let mut bgs = bg.chars();
    let mut run: Option<(char, char)> = None;
    for glyph in glyphs.chars() {
        let key = (fgs.next().unwrap_or(' '), bgs.next().unwrap_or(' '));
        if run != Some(key) {
            if run.is_some_and(|(f, b)| f != ' ' || b != ' ') {
                out.push_str(RESET);
            }
            out.push_str(theme.fg(role(key.0)));
            out.push_str(&theme.bg(role(key.1)));
            run = Some(key);
        }
        out.push(glyph);
    }
    if run.is_some_and(|(f, b)| f != ' ' || b != ' ') {
        out.push_str(RESET);
    }
    out
}

/// The name as the popup writes it in text: `❯ hertrees`, two-tone.
pub fn name_segments() -> Vec<Segment> {
    vec![
        Segment::bold("accent", format!("{} ", crate::render::PROMPT), true),
        Segment::bold("text", "her", true),
        Segment::bold("idle", "trees", true),
    ]
}

/// The rows a resolved style draws above the search line, each padded to
/// `width`; empty for `Search` and `Off`. `version` sits right-aligned and dim
/// on the art's last row (the icon's status line carries it instead).
pub fn render(theme: &Theme, style: Style, width: isize, version: &str, worktrees: usize, agents: usize) -> Vec<String> {
    let Some(art) = style.art() else { return Vec::new() };
    let art_w = art_width(art);
    let mut rows: Vec<String> = art.iter().map(|&row| paint_art_row(theme, row)).collect();
    if style == Style::Icon {
        let room = (width - ICON_TEXT as isize).max(0);
        let plural = |n: usize, noun: &str| format!("{} {}{}", n, noun, if n == 1 { "" } else { "s" });
        let status = format!("v{} · {} · {}", version, plural(worktrees, "worktree"), plural(agents, "agent"));
        let text: [(usize, Vec<Segment>); 4] = [
            (2, name_segments()),
            (3, vec![Segment::new("muted", TAGLINE[0])]),
            (4, vec![Segment::new("muted", TAGLINE[1])]),
            (6, vec![Segment::new("unknown", truncate(&status, room))]),
        ];
        for (index, segments) in text {
            let gap = " ".repeat(ICON_TEXT - display_width(art[index].0));
            rows[index] = format!("{}{}{}", rows[index], gap, segments_to_text(theme, &segments, ""));
        }
        rows.push(String::new());
    } else {
        let tag = format!("v{}", version);
        let tag_w = display_width(&tag) as isize;
        if let Some(last) = rows.last_mut() {
            if art_w + 2 + tag_w <= width {
                let gap = " ".repeat((width - art_w - tag_w) as usize);
                *last = format!("{}{}{}", last, gap, theme.paint("unknown", &tag));
            }
        }
    }
    rows.iter().map(|row| pad(row, width)).collect()
}

// -- art, generated from the brand grids --------------------------------------

const PIXEL: Art = &[
    ("▄    █▄▄ ▄▄▄ ▄▄▄ ▄█▄ ▄▄▄ ▄▄▄ ▄▄▄ ▄▄▄", "a    hhh hhh hhh ttt ttt ttt ttt ttt", ""),
    (" ▀▄  █ █ █▄█ █    █  █   █▄█ █▄█ █▄▄", " aa  h h hhh h    t  t   ttt ttt ttt", ""),
    ("▄▀   █ █ █▄▄ █    █▄ █   █▄▄ █▄▄ ▄▄█", "aa   h h hhh h    tt t   ttt ttt ttt", ""),
];

const PIXEL_SMALL: Art = &[
    ("▄   █▄▄ ▄▄▄ ▄▄▄ ▄█▄ ▄▄▄ ▄▄▄ ▄▄▄  ▄▄", "a   hhh hhh hhh ttt ttt ttt ttt  tt", ""),
    ("▄▀  █ █ ██▄ █    █▄ █   ██▄ ██▄ ▄█ ", "aa  h h hhh h    tt t   ttt ttt tt ", ""),
];

const BRAILLE: Art = &[
    ("⡀ ⣆⣀⢀⣀⡀⣀⡀⣠⡀⣀⡀⣀⣀⢀⣀⡀⣀⣀", "a hhhhhhhttttttttttt", ""),
    ("⡨⠂⡇⢸⢸⣒⡃⡇ ⢸⡀⡇ ⣗⣚⢸⣒⡃⣓⣲", "aahhhhhh ttt ttttttt", ""),
];

const SEXTANT: Art = &[
    ("🬢 🬛🬓🬚🬓🬚🬃🬫🬃🬚🬃🬚🬓🬚🬓🬚🬃", "a hhhhhhtttttttttt", ""),
    ("🬖🬀▌▌🬴🬐▌ ▐🬏▌ 🬴🬐🬴🬐🬰▌", "aahhhhh ttt tttttt", ""),
];

const ICON: Art = &[
    ("          ▄▄▄▄  ", "          wwww  ", ""),
    ("  ▄▄▄▄▄   █ ▄█▄ ", "  aaaaa   m www ", ""),
    (" █▀▀▀▀▀█  █ ███ ", " aaaaaaa  m www ", "   w w          "),
    (" █▄▄▄▄▄█  █     ", " aaaaaaa  m     ", ""),
    (" █▀▀▀▀▀█ ▄█ ██▀█", " aaaaaaa wm tttt", "  wwwww       w "),
    (" ███████  █ ▀███", " aaaaaaa  m tttt", ""),
    ("  ▀   ▀   █  ██ ", "  m   m   m  mm ", ""),
    ("████▀████▀▀▀██▀▀", "uuuummmmmwwwmmmm", "    u    mmm  uu"),
];


#[cfg(test)]
mod tests {
    use super::*;
    use crate::textutil::{strip_ansi, visible_width};

    const ART: [Style; 5] = [Style::Pixel, Style::PixelSmall, Style::Braille, Style::Sextant, Style::Icon];

    #[test]
    fn art_rows_line_up_and_name_known_roles() {
        for style in ART {
            let art = style.art().unwrap();
            let width = display_width(art[0].0);
            for (glyphs, fg, bg) in art {
                assert_eq!(display_width(glyphs), width, "{style:?}: every glyph is one cell");
                assert_eq!(glyphs.chars().count(), width);
                assert_eq!(fg.chars().count(), width, "{style:?}");
                assert!(bg.is_empty() || bg.chars().count() == width, "{style:?}");
                assert!(fg.chars().chain(bg.chars()).all(|c| c == ' ' || !role(c).is_empty()));
                // A drawn cell always has a colour; a blank cell never does.
                for (glyph, code) in glyphs.chars().zip(fg.chars()) {
                    assert_eq!(glyph == ' ', code == ' ', "{style:?} {glyphs:?}");
                }
            }
        }
    }

    #[test]
    fn each_style_draws_its_rows_at_full_width() {
        let theme = Theme::new(None, None, None);
        let expected = [(Style::Pixel, 3), (Style::PixelSmall, 2), (Style::Braille, 2), (Style::Sextant, 2), (Style::Icon, 9)];
        for (style, count) in expected {
            let rows = render(&theme, style, 100, "1.2.3", 3, 1);
            assert_eq!(rows.len(), count, "{style:?}");
            assert_eq!(style.rows(), count as isize);
            assert!(rows.iter().all(|row| visible_width(row) == 100), "{style:?}");
            let plain: Vec<String> = rows.iter().map(|row| strip_ansi(row)).collect();
            assert_eq!(plain[0].trim_end(), style.art().unwrap()[0].0.trim_end(), "{style:?}");
            // Two-tone: the prompt's accent, "her" in text, "trees" in idle.
            let joined = rows.join("\n");
            for role in ["accent", "idle"] {
                assert!(joined.contains(theme.fg(role)), "{style:?} {role}");
            }
            if style == Style::Icon {
                assert_eq!(plain[2].find("❯ hertrees").map(|at| display_width(&plain[2][..at])), Some(ICON_TEXT));
                assert!(plain[3].contains("every worktree, every agent,") && plain[4].contains("one popup."));
                assert!(plain[6].contains("v1.2.3 · 3 worktrees · 1 agent"), "{:?}", plain[6]);
                assert!(plain[8].trim().is_empty());
                assert!(joined.contains(&theme.bg("working")), "the lantern's two-colour cells use a background");
            } else {
                assert!(plain.last().unwrap().ends_with("v1.2.3"), "{style:?}");
            }
        }
        for style in [Style::Search, Style::Off] {
            assert!(render(&theme, style, 100, "1.2.3", 3, 1).is_empty());
            assert_eq!(style.rows(), 0);
        }
    }

    #[test]
    fn the_version_is_left_out_when_it_would_touch_the_art() {
        let theme = Theme::new(None, None, None);
        let width = Style::Pixel.min_width();
        let rows = render(&theme, Style::Pixel, width, "1.2.3", 0, 0);
        assert!(rows.iter().all(|row| visible_width(row) as isize == width));
        assert!(!strip_ansi(rows.last().unwrap()).contains("v1.2.3"));
    }

    #[test]
    fn pixel_styles_fall_back_to_the_search_line_when_short_or_narrow() {
        for style in ART {
            let tall = style.rows() + MIN_LIST_ROWS;
            assert_eq!(style.resolve(100, tall), style, "{style:?}");
            assert_eq!(style.resolve(style.min_width(), tall), style, "{style:?}");
            assert_eq!(style.resolve(100, tall - 1), Style::Search, "{style:?}: short");
            assert_eq!(style.resolve(style.min_width() - 1, tall), Style::Search, "{style:?}: narrow");
        }
        assert_eq!(Style::Icon.min_width(), 46);
        assert_eq!(Style::Pixel.min_width(), 36);
        for style in [Style::Search, Style::Off] {
            assert_eq!(style.resolve(1, 1), style);
        }
    }
}
