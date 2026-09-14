//! Color palettes. Colors are packed RGBA8 (little-endian: r in the low byte) so they can
//! be handed to the GPU untouched.

use crate::lex::{Class, CLASS_COUNT};

pub type Color = u32;

pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24)
}

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    rgba(r, g, b, 255)
}

pub fn to_f64(c: Color) -> [f64; 4] {
    [
        (c & 0xff) as f64 / 255.0,
        ((c >> 8) & 0xff) as f64 / 255.0,
        ((c >> 16) & 0xff) as f64 / 255.0,
        ((c >> 24) & 0xff) as f64 / 255.0,
    ]
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub gutter_bg: Color,
    pub gutter_fg: Color,
    pub gutter_fg_cursor: Color,
    pub divider: Color,
    pub filler_bg: Color,
    pub del_bg: Color,
    pub del_strong: Color,
    pub add_bg: Color,
    pub add_strong: Color,
    pub ws_bg: Color,
    pub ws_strong: Color,
    pub ws_marker: Color,
    pub ws_hidden_marker: Color,
    pub cursor_row: Color,
    pub status_bg: Color,
    pub status_fg: Color,
    pub status_accent: Color,
    pub status_dim: Color,
    pub hunk_marker: Color,
    pub scrollbar: Color,
    pub scrollbar_del: Color,
    pub scrollbar_add: Color,
    pub error_fg: Color,
    pub search_bg: Color,
    pub scrollbar_track: Color,
    pub picker_bg: Color,
    pub picker_border: Color,
    pub picker_selected: Color,
    pub picker_match: Color,
    pub status_added: Color,
    pub status_deleted: Color,
    /// Text color per `lex::Class`; `Plain` is `fg`.
    pub syntax: [Color; CLASS_COUNT],
}

struct Syntax {
    fg: Color,
    comment: Color,
    string: Color,
    escape: Color,
    number: Color,
    keyword: Color,
    ty: Color,
    function: Color,
    attribute: Color,
    lifetime: Color,
    property: Color,
    constant: Color,
    punct: Color,
}

const fn syntax(c: Syntax) -> [Color; CLASS_COUNT] {
    let mut t = [c.fg; CLASS_COUNT];
    t[Class::Comment as usize] = c.comment;
    t[Class::String as usize] = c.string;
    t[Class::Escape as usize] = c.escape;
    t[Class::Number as usize] = c.number;
    t[Class::Keyword as usize] = c.keyword;
    t[Class::Type as usize] = c.ty;
    t[Class::Function as usize] = c.function;
    t[Class::Attribute as usize] = c.attribute;
    t[Class::Lifetime as usize] = c.lifetime;
    t[Class::Property as usize] = c.property;
    t[Class::Constant as usize] = c.constant;
    t[Class::Punct as usize] = c.punct;
    t
}

pub const DARK: Theme = Theme {
    bg: rgb(0x1a, 0x1c, 0x22),
    fg: rgb(0xd4, 0xd8, 0xe0),
    gutter_bg: rgb(0x16, 0x18, 0x1d),
    gutter_fg: rgb(0x5a, 0x60, 0x6e),
    gutter_fg_cursor: rgb(0xc8, 0xcc, 0xd6),
    divider: rgb(0x2c, 0x30, 0x3a),
    filler_bg: rgba(0x2a, 0x2d, 0x36, 0xa0),
    del_bg: rgba(0xf2, 0x5c, 0x6c, 0x2e),
    del_strong: rgba(0xf2, 0x5c, 0x6c, 0x70),
    add_bg: rgba(0x4e, 0xd2, 0x90, 0x28),
    add_strong: rgba(0x4e, 0xd2, 0x90, 0x66),
    ws_bg: rgba(0x6f, 0x9c, 0xf5, 0x22),
    ws_strong: rgba(0x6f, 0x9c, 0xf5, 0x58),
    ws_marker: rgb(0x7f, 0x9c, 0xe0),
    ws_hidden_marker: rgb(0x4a, 0x5a, 0x80),
    cursor_row: rgba(0xff, 0xff, 0xff, 0x12),
    status_bg: rgb(0x23, 0x26, 0x2e),
    status_fg: rgb(0xb8, 0xbe, 0xca),
    status_accent: rgb(0x8a, 0xb4, 0xf8),
    status_dim: rgb(0x6c, 0x72, 0x80),
    hunk_marker: rgb(0x8a, 0xb4, 0xf8),
    scrollbar: rgba(0xa0, 0xa8, 0xb8, 0x70),
    scrollbar_del: rgb(0xd8, 0x5a, 0x66),
    scrollbar_add: rgb(0x4a, 0xb8, 0x80),
    error_fg: rgb(0xf2, 0x7a, 0x7a),
    search_bg: rgba(0xf0, 0xc0, 0x40, 0x60),
    scrollbar_track: rgb(0x14, 0x16, 0x1b),
    picker_bg: rgb(0x25, 0x28, 0x31),
    picker_border: rgb(0x3c, 0x42, 0x50),
    picker_selected: rgba(0x8a, 0xb4, 0xf8, 0x30),
    picker_match: rgb(0xf0, 0xc0, 0x60),
    status_added: rgb(0x5f, 0xd0, 0x90),
    status_deleted: rgb(0xf0, 0x70, 0x78),
    syntax: syntax(Syntax {
        fg: rgb(0xd4, 0xd8, 0xe0),
        comment: rgb(0x7a, 0x82, 0x92),
        string: rgb(0xa8, 0xcc, 0x8c),
        escape: rgb(0x78, 0xcc, 0xc8),
        number: rgb(0xe3, 0xa5, 0x6f),
        keyword: rgb(0xc5, 0x95, 0xe8),
        ty: rgb(0xe8, 0xc6, 0x7e),
        function: rgb(0x80, 0xb6, 0xf2),
        attribute: rgb(0x8e, 0xa8, 0xa0),
        lifetime: rgb(0xe8, 0x96, 0x8a),
        property: rgb(0xa6, 0xc8, 0xda),
        constant: rgb(0xe3, 0xa5, 0x6f),
        punct: rgb(0x9a, 0xa2, 0xb0),
    }),
};

pub const LIGHT: Theme = Theme {
    bg: rgb(0xfb, 0xfb, 0xfc),
    fg: rgb(0x24, 0x28, 0x30),
    gutter_bg: rgb(0xf1, 0xf2, 0xf5),
    gutter_fg: rgb(0xa0, 0xa6, 0xb2),
    gutter_fg_cursor: rgb(0x30, 0x34, 0x40),
    divider: rgb(0xdc, 0xdf, 0xe6),
    filler_bg: rgba(0xe6, 0xe8, 0xee, 0xc0),
    del_bg: rgba(0xf0, 0x40, 0x50, 0x24),
    del_strong: rgba(0xf0, 0x40, 0x50, 0x60),
    add_bg: rgba(0x20, 0xb0, 0x60, 0x22),
    add_strong: rgba(0x20, 0xb0, 0x60, 0x5c),
    ws_bg: rgba(0x40, 0x80, 0xf0, 0x1e),
    ws_strong: rgba(0x40, 0x80, 0xf0, 0x50),
    ws_marker: rgb(0x60, 0x80, 0xc8),
    ws_hidden_marker: rgb(0xb0, 0xbc, 0xd8),
    cursor_row: rgba(0x00, 0x00, 0x40, 0x0e),
    status_bg: rgb(0xe8, 0xea, 0xef),
    status_fg: rgb(0x3a, 0x40, 0x4c),
    status_accent: rgb(0x2a, 0x60, 0xd0),
    status_dim: rgb(0x8a, 0x90, 0x9c),
    hunk_marker: rgb(0x2a, 0x60, 0xd0),
    scrollbar: rgba(0x30, 0x34, 0x40, 0x60),
    scrollbar_del: rgb(0xe0, 0x50, 0x60),
    scrollbar_add: rgb(0x30, 0xa8, 0x70),
    error_fg: rgb(0xc0, 0x30, 0x30),
    search_bg: rgba(0xf0, 0xb0, 0x20, 0x60),
    scrollbar_track: rgb(0xec, 0xee, 0xf2),
    picker_bg: rgb(0xf4, 0xf5, 0xf8),
    picker_border: rgb(0xc8, 0xcc, 0xd6),
    picker_selected: rgba(0x2a, 0x60, 0xd0, 0x28),
    picker_match: rgb(0xb0, 0x60, 0x00),
    status_added: rgb(0x20, 0x90, 0x50),
    status_deleted: rgb(0xc0, 0x30, 0x40),
    syntax: syntax(Syntax {
        fg: rgb(0x24, 0x28, 0x30),
        comment: rgb(0x78, 0x7e, 0x8a),
        string: rgb(0x3a, 0x7a, 0x2a),
        escape: rgb(0x12, 0x7a, 0x84),
        number: rgb(0xa4, 0x52, 0x08),
        keyword: rgb(0x86, 0x38, 0xb4),
        ty: rgb(0x86, 0x5e, 0x00),
        function: rgb(0x22, 0x5c, 0xb4),
        attribute: rgb(0x46, 0x70, 0x68),
        lifetime: rgb(0xac, 0x42, 0x36),
        property: rgb(0x2a, 0x6a, 0x86),
        constant: rgb(0xa4, 0x52, 0x08),
        punct: rgb(0x5a, 0x60, 0x6c),
    }),
};
