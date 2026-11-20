use std::str;

use lapce_xi_rope::{LinesMetric, Rope, spans::Spans};
use planck_rpc::style::{LineStyle, Style};

pub const SCOPES: &[&str] = &[
    "constant",
    "type",
    "type.builtin",
    "property",
    "comment",
    "constructor",
    "function",
    "label",
    "keyword",
    "string",
    "variable",
    "variable.other.member",
    "operator",
    "attribute",
    "escape",
    "embedded",
    "symbol",
    "punctuation",
    "punctuation.special",
    "punctuation.delimiter",
    "text",
    "text.literal",
    "text.title",
    "text.uri",
    "text.reference",
    "string.escape",
    "conceal",
    "none",
    "tag",
    "markup.bold",
    "markup.italic",
    "markup.list",
    "markup.quote",
    "markup.heading",
    "markup.link.url",
    "markup.link.label",
    "markup.link.text",
    "markup.raw",
    "markup.strikethrough",
    "diff.plus",
    "diff.minus",
];

pub fn line_styles(
    text: &Rope,
    line: usize,
    styles: &Spans<Style>,
) -> Vec<LineStyle> {
    let max_line = text.measure::<LinesMetric>() + 1;

    if line >= max_line {
        return Vec::new();
    }

    let start_offset = text.offset_of_line(line);
    let end_offset = text.offset_of_line(line + 1);
    let line_styles: Vec<LineStyle> = styles
        .iter_chunks(start_offset..end_offset)
        .filter_map(|(iv, style)| {
            let start = iv.start();
            let end = iv.end();
            if start > end_offset || end < start_offset {
                None
            } else {
                let start = start.saturating_sub(start_offset);
                let end = end - start_offset;
                let style = style.clone();
                Some(LineStyle { start, end, style })
            }
        })
        .collect();
    line_styles
}

/// Colors for one row of a hex dump (Planck's binary view, or `hexdump -C`):
/// a dim offset, each byte by kind like `hexyl` does, and the ASCII column
/// in the same colors as its bytes. Done per line when the line is drawn, so
/// a 4 MB binary doesn't build styles for all of its rows up front.
pub fn hex_dump_line_styles(line: &str) -> Vec<LineStyle> {
    fn byte_scope(b: u8) -> &'static str {
        match b {
            0x00 => "comment",
            0x21..=0x7e => "type",
            0x01..=0x20 | 0x7f => "function",
            _ => "string",
        }
    }

    let mut styles: Vec<LineStyle> = Vec::new();
    let mut push = |start: usize, end: usize, scope: &str| match styles.last_mut() {
        Some(last)
            if last.end == start
                && last.style.fg_color.as_deref() == Some(scope) =>
        {
            last.end = end
        }
        _ => styles.push(LineStyle {
            start,
            end,
            style: Style {
                fg_color: Some(scope.to_string()),
            },
        }),
    };

    let row = line.trim_end_matches(['\n', '\r']);
    let b = row.as_bytes();
    let offset_end = b.iter().take_while(|c| c.is_ascii_hexdigit()).count();
    if offset_end == 0 {
        // Not a row, like the "first 4 MB shown" note at the end.
        if !row.is_empty() {
            push(0, row.len(), "comment");
        }
        return styles;
    }
    push(0, offset_end, "comment");

    // Bytes are pairs of hex digits, up to the `|` that opens the ASCII column.
    let mut values = Vec::with_capacity(16);
    let mut i = offset_end;
    while i + 1 < b.len() && b[i] != b'|' {
        if b[i].is_ascii_hexdigit() && b[i + 1].is_ascii_hexdigit() {
            let value = u8::from_str_radix(&row[i..i + 2], 16).unwrap_or(0);
            push(i, i + 2, byte_scope(value));
            values.push(value);
            i += 2;
        } else {
            i += 1;
        }
    }

    // The ASCII column: one char per byte between the first `|` and the last.
    if i < b.len() && b[i] == b'|' && row.ends_with('|') && row.len() - 1 > i {
        let close = row.len() - 1;
        push(i, i + 1, "punctuation");
        for (k, pos) in (i + 1..close).enumerate() {
            let scope = match values.get(k) {
                Some(&value) => byte_scope(value),
                None if b[pos] == b'.' => "comment",
                None => "type",
            };
            push(pos, pos + 1, scope);
        }
        push(close, close + 1, "punctuation");
    }
    styles
}

#[cfg(test)]
mod tests {
    use super::hex_dump_line_styles;

    /// Each colored span as "text=scope".
    fn spans(line: &str) -> Vec<String> {
        hex_dump_line_styles(line)
            .into_iter()
            .map(|s| {
                format!("{}={}", &line[s.start..s.end], s.style.fg_color.unwrap())
            })
            .collect()
    }

    #[test]
    fn hex_dump_row_colors_offset_bytes_and_ascii() {
        // Planck's own layout: `G`, NUL, newline, 0xff
        let row =
            "00000010  47 00 0a ff                                       |G...|\n";
        assert_eq!(
            spans(row),
            [
                "00000010=comment",
                "47=type",
                "00=comment",
                "0a=function",
                "ff=string",
                "|=punctuation",
                "G=type",
                ".=comment",
                ".=function",
                ".=string",
                "|=punctuation",
            ]
        );
    }

    #[test]
    fn hex_dump_handles_hexdump_c_and_pipe_bytes() {
        // `hexdump -C` adds a space after 8 bytes, and 0x7c is `|` itself.
        let row = "00000000  7c 7c 41 42 43 44 45 46  48  |||ABCDEFH|";
        let got = spans(row);
        assert_eq!(got.len(), 1 + 9 + 3);
        assert!(got[1..10].iter().all(|s| s.ends_with("=type")));
        assert_eq!(
            got[10..],
            ["|=punctuation", "||ABCDEFH=type", "|=punctuation"]
        );

        let note = "... first 4 MB of 9000000 bytes shown";
        assert_eq!(spans(note), [format!("{note}=comment")]);
    }
}
