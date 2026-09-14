#!/usr/bin/env run-cargo-script
//! Lexer fixture: constructs where lexer-level context decides the highlight. The oracle
//! test checks this file against tree-sitter. It is never compiled.

#![allow(dead_code)]

use std::collections::{self as coll, HashMap};
use std::fmt::Write as _;
use std::str;

pub const MAX_LEN: usize = 10;
static GREETING: &'static str = "hi\tthere\u{1F600}\x41 \
                                 continued";
type Pair = (u32, &'static str);

/* block /* nested */ comment */

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Point<T: Copy + Default = u8> {
    pub x: T,
    pub(crate) y: T,
    #[doc = "z"]
    z: Option<Box<dyn Fn(T) -> T + Send + 'static>>,
}

pub struct Wrapper(pub u8, Vec<(u16, u32)>);

pub enum Shape {
    Circle(f64),
    Rect { w: f64, h: f64 },
    Empty,
}

impl<T: Copy + Default> Point<T> {
    pub fn new(x: T, y: T) -> Self {
        Self { x, y, z: None }
    }

    fn bool(&self) -> bool {
        true
    }
}

trait Area {
    type Output;
    const SIDES: u32;
    fn area(&self) -> Self::Output;
}

fn generic<'a, F>(f: F, s: &'a str) -> impl Iterator<Item = &'a str> + use<'a, F>
where
    F: for<'r> FnOnce(&'r str) -> bool,
{
    s.split(',')
}

macro_rules! square {
    ($x:expr, $T:ty) => {
        $x * $x as $T
    };
}

async fn everything(shape: Shape, items: &[u64], r#type: u8) -> Result<u32, String> {
    let n = 1.5e3 + 0x_ffu8 as f64 + 1_000i64 as f64;
    let t = (1, (2, 3));
    let inner = t.1.0;
    let c = '\n';
    let b = b'x';
    let bs = b"bytes\0";
    let raw = r#"raw "quoted" string"#;
    let ch = 'λ';
    let w: Vec<_> = items.iter().map(|&v| v * 2).collect::<Vec<u64>>();
    let v = Vec::<u8>::with_capacity(4);
    let total: u64 = items.iter().copied().sum();
    let (a, b): (u8, u16) = (1, 2);
    let Some(first) = items.first() else {
        return Err(format!("empty: {}", items.len()));
    };
    let ok = matches!(shape, Shape::Circle(r) if r > 0.0);
    let area = match shape {
        Shape::Circle(r) => r * r * std::f64::consts::PI,
        Shape::Rect { w, h } if w > 0.0 => w * h,
        Shape::Rect { .. } => 0.0,
        Shape::Empty => {
            println!("empty");
            0.0
        }
    };
    'outer: for (i, x) in items.iter().enumerate() {
        if *x as usize <= i || i < MAX_LEN {
            continue 'outer;
        }
        while let Some(y) = Some(*x) {
            break 'outer;
        }
    }
    let closure = |a: u8, b: u8| -> u16 { a as u16 | b as u16 };
    let p = Point::<u8> { x: 1, y: 2, z: None };
    let q = Point::new(1u8, 2u8).x;
    let z = u8::MAX.min(r#type);
    let sq = square!(3, u32);
    let mut map: HashMap<String, Vec<u8>> = HashMap::new();
    map.entry("k".to_string()).or_default().push(q);
    let s = str::from_utf8(bs).unwrap_or_default();
    let fut = async move { total }.await;
    Ok(sq + inner as u32 + *first as u32 + ok as u32 + area as u32 + fut as u32 + z as u32)
}
