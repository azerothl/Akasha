//! Theme-aware markdown renderer (pulldown-cmark). Tables, lists, code blocks, **bold**, *italic*, `code`.
//! Inspired by https://github.com/BrendanGraham14/steer/blob/main/crates/steer-tui/src/tui/widgets/markdown.rs

use crate::theme::ThemePalette;
use itertools::Itertools;
use pulldown_cmark::{CodeBlockKind, CowStr, Event, HeadingLevel, Options, Parser, Tag};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::iter::Peekable;
use unicode_width::UnicodeWidthStr;

/// A line with optional no-wrap (e.g. code block) and indent for list wrapping.
#[derive(Debug, Clone)]
#[allow(dead_code)] // no_wrap/indent_level kept for future wrap-aware rendering
pub struct MarkedLine {
    pub line: Line<'static>,
    pub no_wrap: bool,
    pub indent_level: usize,
}

impl MarkedLine {
    #[allow(dead_code)]
    pub fn new(line: Line<'static>) -> Self {
        Self {
            line,
            no_wrap: false,
            indent_level: 0,
        }
    }

    #[allow(dead_code)]
    pub fn new_no_wrap(line: Line<'static>) -> Self {
        Self {
            line,
            no_wrap: true,
            indent_level: 0,
        }
    }
}

#[derive(Debug, Default)]
pub struct MarkedText {
    pub lines: Vec<MarkedLine>,
}

/// Markdown styles (from theme palette).
#[derive(Debug, Clone)]
pub struct MarkdownStyles {
    pub h1: Style,
    pub h2: Style,
    pub h3: Style,
    pub h4: Style,
    pub h5: Style,
    pub h6: Style,
    pub emphasis: Style,
    pub strong: Style,
    pub strikethrough: Style,
    pub blockquote: Style,
    pub code: Style,
    pub code_block: Style,
    pub link: Style,
    pub list_marker: Style,
    pub list_number: Style,
    pub table_border: Style,
    pub table_header: Style,
    pub table_cell: Style,
    pub task_checked: Style,
    pub task_unchecked: Style,
}

impl MarkdownStyles {
    pub fn from_palette(p: &ThemePalette) -> Self {
        Self {
            h1: Style::default().fg(p.accent).add_modifier(Modifier::BOLD | Modifier::REVERSED),
            h2: Style::default().fg(p.accent).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            h3: Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            h4: Style::default().fg(p.info).add_modifier(Modifier::UNDERLINED),
            h5: Style::default().fg(p.info).add_modifier(Modifier::ITALIC),
            h6: Style::default().fg(p.muted).add_modifier(Modifier::ITALIC),
            emphasis: Style::default().fg(p.fg).add_modifier(Modifier::ITALIC),
            strong: Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
            strikethrough: Style::default().add_modifier(Modifier::CROSSED_OUT),
            blockquote: Style::default().fg(p.muted).add_modifier(Modifier::ITALIC),
            code: Style::default().fg(p.accent),
            code_block: Style::default().fg(p.muted),
            link: Style::default().fg(p.info).add_modifier(Modifier::UNDERLINED),
            list_marker: Style::default().fg(p.warning),
            list_number: Style::default().fg(p.warning),
            table_border: Style::default().fg(p.muted),
            table_header: Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            table_cell: Style::default().fg(p.fg),
            task_checked: Style::default().fg(p.success),
            task_unchecked: Style::default().fg(p.muted),
        }
    }
}

/// Render markdown to marked lines. Use `to_flat_lines()` for Paragraph.
#[allow(dead_code)]
pub fn from_str(input: &str, styles: &MarkdownStyles) -> MarkedText {
    from_str_with_width(input, styles, None)
}

pub fn from_str_with_width(
    input: &str,
    styles: &MarkdownStyles,
    terminal_width: Option<u16>,
) -> MarkedText {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);
    let parser = Parser::new_ext(input, options);
    let mut writer = TextWriter::new(parser, styles);
    writer.terminal_width = terminal_width;
    writer.run();
    writer.marked_text
}

impl MarkedText {
    /// Flatten to Vec<Line> for Paragraph (no_wrap lines are single Line; indent not applied here, Paragraph wraps).
    pub fn to_flat_lines(&self) -> Vec<Line<'static>> {
        self.lines.iter().map(|m| m.line.clone()).collect()
    }
}

struct TextWriter<'a, I>
where
    I: Iterator<Item = Event<'a>>,
{
    iter: Peekable<I>,
    marked_text: MarkedText,
    inline_styles: Vec<Style>,
    line_prefixes: Vec<Span<'static>>,
    line_styles: Vec<Style>,
    list_indices: Vec<Option<u64>>,
    link: Option<String>,
    needs_newline: bool,
    styles: &'a MarkdownStyles,
    table_alignments: Vec<pulldown_cmark::Alignment>,
    table_rows: Vec<Vec<Vec<Span<'static>>>>,
    in_table_header: bool,
    in_list_item_start: bool,
    in_code_block: bool,
    code_block_language: Option<String>,
    terminal_width: Option<u16>,
    list_item_indent: usize,
}

impl<'a, I> TextWriter<'a, I>
where
    I: Iterator<Item = Event<'a>>,
{
    fn new(iter: I, styles: &'a MarkdownStyles) -> Self {
        Self {
            iter: iter.peekable(),
            marked_text: MarkedText::default(),
            inline_styles: vec![],
            line_prefixes: vec![],
            line_styles: vec![],
            list_indices: vec![],
            link: None,
            needs_newline: false,
            styles,
            table_alignments: vec![],
            table_rows: vec![],
            in_table_header: false,
            in_list_item_start: false,
            in_code_block: false,
            code_block_language: None,
            terminal_width: None,
            list_item_indent: 0,
        }
    }

    fn run(&mut self) {
        while let Some(event) = self.iter.next() {
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: Event<'a>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.text(text),
            Event::Code(code) => self.code(code),
            Event::Html(html) => self.text(CowStr::from(html.to_string())),
            Event::FootnoteReference(_) => {}
            Event::SoftBreak => self.soft_break(),
            Event::HardBreak => self.hard_break(),
            Event::Rule => self.rule(),
            Event::TaskListMarker(checked) => self.task_list_marker(checked),
        }
    }

    fn start_tag(&mut self, tag: Tag<'a>) {
        match tag {
            Tag::Paragraph => self.start_paragraph(),
            Tag::Heading(level, _, _) => self.start_heading(level),
            Tag::BlockQuote => self.start_blockquote(),
            Tag::CodeBlock(kind) => self.start_codeblock(kind),
            Tag::List(start_index) => self.start_list(start_index),
            Tag::Item => self.start_item(),
            Tag::FootnoteDefinition(_) => {}
            Tag::Table(alignments) => self.start_table(alignments),
            Tag::TableHead => self.start_table_head(),
            Tag::TableRow => self.start_table_row(),
            Tag::TableCell => self.start_table_cell(),
            Tag::Emphasis | Tag::Strong | Tag::Strikethrough => {
                if self.in_list_item_start {
                    self.push_list_marker();
                    self.in_list_item_start = false;
                }
                match tag {
                    Tag::Emphasis => self.push_inline_style(self.styles.emphasis),
                    Tag::Strong => self.push_inline_style(self.styles.strong),
                    Tag::Strikethrough => self.push_inline_style(self.styles.strikethrough),
                    _ => {}
                }
            }
            Tag::Link(_link_type, dest_url, _title) => {
                if self.in_list_item_start {
                    self.push_list_marker();
                    self.in_list_item_start = false;
                }
                self.link = Some(dest_url.to_string());
            }
            Tag::Image(_, _, _) => {}
        }
    }

    fn end_tag(&mut self, tag: Tag<'a>) {
        match tag {
            Tag::Paragraph => self.needs_newline = true,
            Tag::Heading(..) => {
                self.pop_inline_style();
                self.needs_newline = true;
            }
            Tag::BlockQuote => {
                self.line_prefixes.pop();
                self.line_styles.pop();
                self.needs_newline = true;
            }
            Tag::CodeBlock(_) => {
                self.in_code_block = false;
                self.code_block_language = None;
                self.line_styles.pop();
                self.needs_newline = true;
            }
            Tag::List(_) => {
                self.list_indices.pop();
                self.needs_newline = true;
            }
            Tag::Item => {
                if self.in_list_item_start {
                    self.push_list_marker();
                    self.in_list_item_start = false;
                }
            }
            Tag::FootnoteDefinition(_) => {}
            Tag::Table(_) => {
                self.render_table();
                self.table_alignments.clear();
                self.table_rows.clear();
                self.needs_newline = true;
            }
            Tag::TableHead => self.in_table_header = false,
            Tag::TableRow | Tag::TableCell => {}
            Tag::Emphasis | Tag::Strong | Tag::Strikethrough => self.pop_inline_style(),
            Tag::Link(..) => self.pop_link(),
            Tag::Image(..) => {}
        }
    }

    fn start_paragraph(&mut self) {
        if self.needs_newline {
            self.push_line(Line::default(), false);
        }
        self.push_line(Line::default(), false);
        self.needs_newline = false;
    }

    fn start_heading(&mut self, level: HeadingLevel) {
        if self.needs_newline {
            self.push_line(Line::default(), false);
        }
        let style = match level {
            HeadingLevel::H1 => self.styles.h1,
            HeadingLevel::H2 => self.styles.h2,
            HeadingLevel::H3 => self.styles.h3,
            HeadingLevel::H4 => self.styles.h4,
            HeadingLevel::H5 => self.styles.h5,
            HeadingLevel::H6 => self.styles.h6,
        };
        self.push_inline_style(style);
        let content = format!("{} ", "#".repeat(level as usize));
        self.push_line(Line::from(Span::styled(content, style)), false);
        self.needs_newline = false;
    }

    fn start_blockquote(&mut self) {
        if self.needs_newline {
            self.push_line(Line::default(), false);
            self.needs_newline = false;
        }
        self.line_prefixes.push(Span::from(">"));
        self.line_styles.push(self.styles.blockquote);
    }

    fn text(&mut self, text: CowStr<'a>) {
        if self.in_list_item_start {
            self.push_list_marker();
            self.in_list_item_start = false;
        }

        let in_table = self.table_rows.last().and_then(|row| row.last()).is_some();

        if in_table {
            let style = self.inline_styles.last().copied().unwrap_or_default();
            self.push_span(Span::styled(text.to_string(), style));
        } else if self.in_code_block {
            let base_style = self
                .inline_styles
                .last()
                .copied()
                .unwrap_or_default()
                .patch(self.styles.code_block);
            for (idx, line) in text.lines().enumerate() {
                if idx > 0 || self.needs_newline {
                    self.push_line(Line::default(), true);
                }
                self.push_span(Span::styled(line.to_string(), base_style));
            }
            self.needs_newline = text.ends_with('\n') && !text.is_empty();
        } else {
            for (position, line) in text.lines().with_position() {
                if self.needs_newline {
                    self.push_line(Line::default(), false);
                    self.needs_newline = false;
                }
                if matches!(position, itertools::Position::Middle | itertools::Position::Last) {
                    self.push_line(Line::default(), false);
                }
                let style = self.inline_styles.last().copied().unwrap_or_default();
                self.push_span(Span::styled(line.to_owned(), style));
            }
            self.needs_newline = false;
        }
    }

    fn code(&mut self, code: CowStr<'a>) {
        if self.in_list_item_start {
            self.push_list_marker();
            self.in_list_item_start = false;
        }
        self.push_span(Span::styled(code.to_string(), self.styles.code));
    }

    fn hard_break(&mut self) {
        self.push_span(Span::from("  "));
        self.push_line(Line::default(), false);
    }

    fn soft_break(&mut self) {
        self.push_line(Line::default(), false);
    }

    fn start_list(&mut self, start_index: Option<u64>) {
        if self.list_indices.is_empty() && self.needs_newline {
            self.push_line(Line::default(), false);
        }
        self.list_indices.push(start_index);
    }

    fn start_item(&mut self) {
        self.push_line(Line::default(), false);
        self.in_list_item_start = true;
        self.list_item_indent = 0;
        self.needs_newline = false;
    }

    fn start_table(&mut self, alignments: Vec<pulldown_cmark::Alignment>) {
        if self.needs_newline {
            self.push_line(Line::default(), false);
        }
        self.table_alignments = alignments;
        self.table_rows.clear();
        self.needs_newline = false;
    }

    fn start_table_head(&mut self) {
        self.in_table_header = true;
        self.table_rows.push(Vec::new());
    }

    #[allow(dead_code)]
    fn end_table_head(&mut self) {
        self.in_table_header = false;
    }

    fn start_table_row(&mut self) {
        self.table_rows.push(Vec::new());
    }

    fn start_table_cell(&mut self) {
        if let Some(row) = self.table_rows.last_mut() {
            row.push(Vec::new());
        }
    }

    fn start_codeblock(&mut self, kind: CodeBlockKind<'a>) {
        if !self.marked_text.lines.is_empty() {
            self.push_line(Line::default(), true);
        }
        self.in_code_block = true;
        self.code_block_language = match kind {
            CodeBlockKind::Fenced(lang) => {
                let s = lang.as_ref();
                if s.is_empty() {
                    None
                } else {
                    Some(s.to_string())
                }
            }
            CodeBlockKind::Indented => None,
        };
        self.line_styles.push(self.styles.code_block);
        self.needs_newline = false;
    }

    fn rule(&mut self) {
        if self.needs_newline {
            self.push_line(Line::default(), false);
        }
        let w = self.terminal_width.unwrap_or(80) as usize;
        let rule = "─".repeat(w);
        self.push_line(
            Line::from(Span::styled(rule, self.styles.blockquote)),
            false,
        );
        self.needs_newline = true;
    }

    fn push_inline_style(&mut self, style: Style) {
        let current = self.inline_styles.last().copied().unwrap_or_default();
        self.inline_styles.push(current.patch(style));
    }

    fn pop_inline_style(&mut self) {
        self.inline_styles.pop();
    }

    fn push_line(&mut self, mut line: Line<'static>, no_wrap: bool) {
        let style = self.line_styles.last().copied().unwrap_or_default();
        line = line.patch_style(style);

        if !self.line_prefixes.is_empty() {
            line.spans.insert(0, Span::from(" "));
            for prefix in self.line_prefixes.iter().rev().cloned() {
                line.spans.insert(0, prefix);
            }
        }

        let marked = MarkedLine {
            line,
            no_wrap,
            indent_level: self.list_item_indent,
        };
        self.marked_text.lines.push(marked);
    }

    fn push_span(&mut self, span: Span<'static>) {
        let in_table = self.table_rows.last().and_then(|row| row.last()).is_some();
        if in_table {
            if let Some(row) = self.table_rows.last_mut() {
                if let Some(cell) = row.last_mut() {
                    cell.push(span);
                }
            }
        } else if let Some(marked) = self.marked_text.lines.last_mut() {
            marked.line.push_span(span);
        } else {
            self.push_line(Line::from(span), false);
        }
    }

    #[allow(dead_code)]
    fn push_link(&mut self, dest_url: CowStr<'a>) {
        self.link = Some(dest_url.to_string());
    }

    fn pop_link(&mut self) {
        if let Some(link) = self.link.take() {
            self.push_span(Span::from(" ("));
            self.push_span(Span::styled(link, self.styles.link));
            self.push_span(Span::from(")"));
        }
    }

    fn push_list_marker(&mut self) {
        if self.list_indices.is_empty() {
            return;
        }
        let depth = self.list_indices.len();
        let indent_width = depth.saturating_sub(1) * 4;
        let indent_str = " ".repeat(indent_width);

        if let Some(last) = self.list_indices.last_mut() {
            let (span, width) = match last {
                None => {
                    let full = format!("{indent_str}- ");
                    (Span::styled(full.clone(), self.styles.list_marker), full.len())
                }
                Some(index) => {
                    *index += 1;
                    let full = format!("{}{}. ", indent_str, *index - 1);
                    (Span::styled(full.clone(), self.styles.list_number), full.len())
                }
            };
            self.list_item_indent = width;
            if let Some(marked) = self.marked_text.lines.last_mut() {
                marked.indent_level = width;
            }
            self.push_span(span);
        }
    }

    fn task_list_marker(&mut self, checked: bool) {
        if self.list_indices.is_empty() {
            return;
        }
        let depth = self.list_indices.len();
        let indent_width = depth.saturating_sub(1) * 4;
        let indent_str = " ".repeat(indent_width);
        let checkbox = if checked { "[✓] " } else { "[ ] " };
        let style = if checked {
            self.styles.task_checked
        } else {
            self.styles.task_unchecked
        };
        let full = format!("{indent_str}- {checkbox}");
        self.list_item_indent = full.len();
        if let Some(marked) = self.marked_text.lines.last_mut() {
            marked.indent_level = full.len();
        }
        self.push_span(Span::styled(full, style));
        self.in_list_item_start = false;
    }

    fn render_table(&mut self) {
        if self.table_rows.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.table_rows);
        let num_cols = self.table_alignments.len();
        let mut col_widths = vec![0; num_cols];

        for row in &rows {
            for (col_idx, cell) in row.iter().enumerate() {
                if col_idx < num_cols {
                    let w: usize = cell
                        .iter()
                        .map(|s| s.content.as_ref().width())
                        .sum();
                    col_widths[col_idx] = col_widths[col_idx].max(w);
                }
            }
        }
        for w in &mut col_widths {
            *w += 2;
        }

        let border = self.styles.table_border;
        let header = self.styles.table_header;
        let cell = self.styles.table_cell;

        self.render_table_border(&col_widths, '┌', '┬', '┐', border);

        for (row_idx, row) in rows.iter().enumerate() {
            let is_header = row_idx == 0 && rows.len() > 1;
            let mut line_spans = vec![Span::styled("│", border)];

            for (col_idx, cell_spans) in row.iter().enumerate() {
                if col_idx < num_cols {
                    let cell_text: String = cell_spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<Vec<_>>()
                        .join("");
                    let align = self
                        .table_alignments
                        .get(col_idx)
                        .copied()
                        .unwrap_or(pulldown_cmark::Alignment::None);
                    let padded = align_text(&cell_text, col_widths[col_idx], align);
                    let style = if is_header { header } else { cell };
                    line_spans.push(Span::styled(padded, style));
                    line_spans.push(Span::styled("│", border));
                }
            }
            self.push_line(Line::from(line_spans), false);
            if is_header {
                self.render_table_border(&col_widths, '├', '┼', '┤', border);
            }
        }
        self.render_table_border(&col_widths, '└', '┴', '┘', border);
    }

    fn render_table_border(
        &mut self,
        col_widths: &[usize],
        left: char,
        mid: char,
        right: char,
        style: Style,
    ) {
        let mut border = String::from(left);
        for (idx, &w) in col_widths.iter().enumerate() {
            border.push_str(&"─".repeat(w));
            if idx < col_widths.len() - 1 {
                border.push(mid);
            }
        }
        border.push(right);
        self.push_line(Line::from(Span::styled(border, style)), false);
    }
}

fn align_text(text: &str, width: usize, alignment: pulldown_cmark::Alignment) -> String {
    let text_len = text.width();
    let total_padding = width.saturating_sub(text_len);

    match alignment {
        pulldown_cmark::Alignment::None | pulldown_cmark::Alignment::Left => {
            let right = total_padding.saturating_sub(1);
            format!(" {}{}", text, " ".repeat(right))
        }
        pulldown_cmark::Alignment::Center => {
            let left = total_padding / 2;
            let right = total_padding - left;
            format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
        }
        pulldown_cmark::Alignment::Right => {
            let left = total_padding.saturating_sub(1);
            format!("{}{} ", " ".repeat(left), text)
        }
    }
}

impl MarkedLine {
    #[allow(dead_code)]
    pub fn with_indent(mut self, indent: usize) -> Self {
        self.indent_level = indent;
        self
    }
}
