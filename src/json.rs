//! JSON reports: a value type, compact and pretty writers, a `json!` macro
//! and a parser, producing byte-for-byte the output serde_json 1.0 gave the
//! engine and its tools before (and parsing what they read).
//!
//! * Structs are written field by field in declaration order (the derive
//!   order); inside a [`Value`] objects are sorted by key, as serde_json's
//!   default `Map` was, including structs converted with [`to_value`].
//! * Numbers: integers in decimal; floats as the shortest representation
//!   that round-trips (Rust's formatter and ryu both produce it), laid out
//!   as ryu did: `1.0`, `0.001`, `1e-7`, `1.5e300`; f32 struct fields use
//!   f32 digits and thresholds, f32 inside a `Value` is widened to f64 as
//!   serde_json did. Non-finite numbers are `null`.
//! * Strings escape `"`, `\` and control characters only (`\n` etc., else
//!   `\u00xx` in lowercase hex).
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub type Map = BTreeMap<String, Value>;

#[derive(Clone, Copy, Debug, PartialEq)]
enum N {
    PosInt(u64),
    NegInt(i64),
    Float(f64),
}

/// A JSON number: an unsigned or negative integer, or a finite float.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Number(N);

impl Number {
    pub fn from_f64(f: f64) -> Option<Number> {
        f.is_finite().then_some(Number(N::Float(f)))
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self.0 {
            N::PosInt(n) => Some(n),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self.0 {
            N::PosInt(n) => i64::try_from(n).ok(),
            N::NegInt(n) => Some(n),
            N::Float(_) => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        Some(match self.0 {
            N::PosInt(n) => n as f64,
            N::NegInt(n) => n as f64,
            N::Float(f) => f,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Value>),
    Object(Map),
}

static NULL: Value = Value::Null;

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(map) => map.get(key),
            _ => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(n) => n.as_u64(),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Number(n) => n.as_i64(),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => n.as_f64(),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&Map> {
        match self {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }
    pub fn as_object_mut(&mut self) -> Option<&mut Map> {
        match self {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }
    pub fn take(&mut self) -> Value {
        std::mem::take(self)
    }
}

impl std::ops::Index<&str> for Value {
    type Output = Value;
    /// The member, or `null` when absent or not an object.
    fn index(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&NULL)
    }
}
impl std::ops::Index<usize> for Value {
    type Output = Value;
    fn index(&self, i: usize) -> &Value {
        match self {
            Value::Array(a) => a.get(i).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
}
impl std::ops::IndexMut<&str> for Value {
    /// The member, inserted as `null` if absent; `null` becomes an object.
    fn index_mut(&mut self, key: &str) -> &mut Value {
        if self.is_null() {
            *self = Value::Object(Map::new());
        }
        match self {
            Value::Object(map) => map.entry(key.to_owned()).or_insert(Value::Null),
            _ => panic!("cannot index a non-object JSON value with a key"),
        }
    }
}

macro_rules! eq_number {
    ($($t:ty => $conv:ident),*) => {$(
        impl PartialEq<$t> for Value {
            fn eq(&self, other: &$t) -> bool {
                self.$conv() == Some(*other as _)
            }
        }
    )*};
}
eq_number!(u8 => as_u64, u16 => as_u64, u32 => as_u64, u64 => as_u64, usize => as_u64,
    i8 => as_i64, i16 => as_i64, i32 => as_i64, i64 => as_i64, f32 => as_f64, f64 => as_f64);
impl PartialEq<bool> for Value {
    fn eq(&self, other: &bool) -> bool {
        self.as_bool() == Some(*other)
    }
}
impl PartialEq<str> for Value {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == Some(other)
    }
}
impl PartialEq<&str> for Value {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == Some(*other)
    }
}
impl PartialEq<String> for Value {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == Some(other.as_str())
    }
}

/// JSON text being written: kept in memory, or passed to a writer in
/// 64 KiB pieces so that large reports are not held whole.
pub struct Out<'a> {
    buf: Vec<u8>,
    sink: Option<&'a mut dyn std::io::Write>,
    error: Option<std::io::Error>,
}
impl<'a> Out<'a> {
    pub fn new() -> Out<'static> {
        Out {
            buf: Vec::new(),
            sink: None,
            error: None,
        }
    }
    pub fn to_writer(sink: &'a mut dyn std::io::Write) -> Self {
        Out {
            buf: Vec::with_capacity(1 << 16),
            sink: Some(sink),
            error: None,
        }
    }
    #[inline]
    pub fn push(&mut self, b: u8) {
        self.buf.push(b);
    }
    #[inline]
    pub fn extend_from_slice(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }
    /// Pass the text so far to the writer once 64 KiB have accumulated.
    #[inline]
    fn spill(&mut self) {
        if self.buf.len() >= 1 << 16 {
            if let Some(sink) = &mut self.sink {
                if self.error.is_none() {
                    self.error = sink.write_all(&self.buf).err();
                }
                self.buf.clear();
            }
        }
    }
    /// Write what remains to the writer.
    pub fn finish(mut self) -> std::io::Result<()> {
        if let Some(sink) = &mut self.sink {
            if self.error.is_none() {
                self.error = sink.write_all(&self.buf).err();
            }
        }
        self.error.map_or(Ok(()), Err)
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}
impl Default for Out<'static> {
    fn default() -> Self {
        Out::new()
    }
}

/// Conversion to JSON: written directly (`write_json`) or as a [`Value`].
pub trait ToJson {
    fn write_json(&self, out: &mut Out);
    fn to_value(&self) -> Value;
}

/// `value` as a [`Value`] (struct members sorted by key).
pub fn to_value<T: ToJson + ?Sized>(value: &T) -> Value {
    value.to_value()
}

/// Compact JSON text.
pub fn to_vec<T: ToJson + ?Sized>(value: &T) -> Vec<u8> {
    let mut out = Out::new();
    value.write_json(&mut out);
    out.into_bytes()
}
/// Compact JSON text passed to `writer` as it is produced.
pub fn to_writer<T: ToJson + ?Sized>(
    writer: &mut dyn std::io::Write,
    value: &T,
) -> std::io::Result<()> {
    let mut out = Out::to_writer(writer);
    value.write_json(&mut out);
    out.finish()
}
pub fn to_string<T: ToJson + ?Sized>(value: &T) -> String {
    String::from_utf8(to_vec(value)).expect("JSON output is UTF-8")
}

/// JSON text indented by two spaces, as serde_json's pretty printer.
pub fn to_string_pretty(value: &Value) -> String {
    let mut out = Out::new();
    write_pretty(value, &mut out, 0);
    String::from_utf8(out.into_bytes()).expect("JSON output is UTF-8")
}

fn write_pretty(value: &Value, out: &mut Out, depth: usize) {
    let indent = |out: &mut Out, depth: usize| {
        for _ in 0..depth {
            out.extend_from_slice(b"  ");
        }
    };
    match value {
        Value::Array(items) if !items.is_empty() => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                out.extend_from_slice(if i == 0 { b"\n" } else { b",\n" });
                indent(out, depth + 1);
                write_pretty(item, out, depth + 1);
            }
            out.push(b'\n');
            indent(out, depth);
            out.push(b']');
        }
        Value::Object(map) if !map.is_empty() => {
            out.push(b'{');
            for (i, (key, item)) in map.iter().enumerate() {
                out.extend_from_slice(if i == 0 { b"\n" } else { b",\n" });
                indent(out, depth + 1);
                write_str(key, out);
                out.extend_from_slice(b": ");
                write_pretty(item, out, depth + 1);
            }
            out.push(b'\n');
            indent(out, depth);
            out.push(b'}');
        }
        other => other.write_json(out),
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&to_string(self))
    }
}

pub fn write_str(s: &str, out: &mut Out) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(b'"');
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let escape: &[u8] = match b {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            0x08 => b"\\b",
            b'\t' => b"\\t",
            b'\n' => b"\\n",
            0x0c => b"\\f",
            b'\r' => b"\\r",
            0..=0x1f => &[],
            _ => continue,
        };
        out.extend_from_slice(&bytes[start..i]);
        if escape.is_empty() {
            out.extend_from_slice(&[
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX[(b >> 4) as usize],
                HEX[(b & 15) as usize],
            ]);
        } else {
            out.extend_from_slice(escape);
        }
        start = i + 1;
    }
    out.extend_from_slice(&bytes[start..]);
    out.push(b'"');
}

/// ryu's layout of the shortest round-trip digits `digits` with the value
/// in [10^exponent, 10^(exponent+1)). `max` is the largest decimal exponent
/// written positionally (16 for f64, 13 for f32) and `min` the smallest
/// written as `0.000ddd` (-5 for f64, -6 for f32).
fn write_float(negative: bool, digits: &[u8], exponent: i32, max: i32, min: i32, out: &mut Out) {
    let length = digits.len() as i32;
    // 10^(kk-1) <= |v| < 10^kk.
    let kk = exponent + 1;
    let k = kk - length;
    if negative {
        out.push(b'-');
    }
    if 0 <= k && kk <= max {
        out.extend_from_slice(digits);
        for _ in 0..k {
            out.push(b'0');
        }
        out.extend_from_slice(b".0");
    } else if 0 < kk && kk <= max {
        out.extend_from_slice(&digits[..kk as usize]);
        out.push(b'.');
        out.extend_from_slice(&digits[kk as usize..]);
    } else if min < kk && kk <= 0 {
        out.extend_from_slice(b"0.");
        for _ in kk..0 {
            out.push(b'0');
        }
        out.extend_from_slice(digits);
    } else {
        out.push(digits[0]);
        if length > 1 {
            out.push(b'.');
            out.extend_from_slice(&digits[1..]);
        }
        out.push(b'e');
        write_integer(exponent < 0, exponent.unsigned_abs() as u64, out);
    }
}

/// Rust's `{:e}` text of a float, formatted on the stack.
struct Scientific {
    buf: [u8; 32],
    len: usize,
}
impl Scientific {
    fn new(args: std::fmt::Arguments) -> Self {
        let mut s = Scientific {
            buf: [0; 32],
            len: 0,
        };
        s.write_fmt(args).expect("float fits");
        s
    }
    /// (negative, digits without the point, digit count, decimal exponent)
    /// of "-d.ddde-7" or "de5".
    fn parts(&self) -> (bool, [u8; 24], usize, i32) {
        let text = &self.buf[..self.len];
        let negative = text[0] == b'-';
        let mut digits = [0u8; 24];
        let mut length = 0;
        let mut i = negative as usize;
        while text[i] != b'e' {
            if text[i] != b'.' {
                digits[length] = text[i];
                length += 1;
            }
            i += 1;
        }
        i += 1;
        let minus = text[i] == b'-';
        i += minus as usize;
        let mut exponent = 0i32;
        for &d in &text[i..] {
            exponent = exponent * 10 + (d - b'0') as i32;
        }
        (
            negative,
            digits,
            length,
            if minus { -exponent } else { exponent },
        )
    }
}
impl std::fmt::Write for Scientific {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let end = self.len + s.len();
        self.buf
            .get_mut(self.len..end)
            .ok_or(std::fmt::Error)?
            .copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Shortest round-trip digits of `scientific` (Rust's `{:e}` output, which
/// picks the representation closest to the exact value), with an exact tie
/// between two closest candidates resolved to the even digit, as ryu does.
/// `exact(n)` formats the value correctly rounded to n + 1 digits;
/// `round_trips` parses a candidate back.
fn ryu_digits(
    scientific: String,
    may_tie: impl Fn(i32, usize) -> bool,
    exact: impl Fn(usize) -> String,
    round_trips: impl Fn(&str) -> bool,
) -> String {
    let split = |s: &str| -> (bool, Vec<u8>, i32) {
        let (m, e) = s.split_once('e').expect("exponent");
        let (negative, m) = m.strip_prefix('-').map_or((false, m), |m| (true, m));
        (
            negative,
            m.bytes().filter(|&b| b != b'.').collect(),
            e.parse().expect("exponent"),
        )
    };
    let (negative, digits, exponent) = split(&scientific);
    let length = digits.len();
    if !may_tie(exponent, length) {
        return scientific;
    }
    // A tie needs digit `length` to be 5 followed only by zeros; a short
    // look-ahead rules out almost every value before the exact expansion.
    let (_, near, near_exponent) = split(&exact(length + 2));
    if near_exponent != exponent || near[length] != b'5' || near[length + 1..] != *b"00" {
        return scientific;
    }
    // Every f64 has at most 767 significant digits.
    let (_, full, _) = split(&exact(780));
    if full[length..].iter().skip(1).any(|&d| d != b'0') || full[length] != b'5' {
        return scientific;
    }
    // Exact tie between full[..length] and full[..length] + 1: take the even
    // one (a carry out of the leading digit is left to Rust's choice).
    let mut even = full[..length].to_vec();
    if even[length - 1] % 2 == 1 {
        let mut i = length;
        loop {
            if i == 0 {
                return scientific;
            }
            i -= 1;
            if even[i] == b'9' {
                even[i] = b'0';
            } else {
                even[i] += 1;
                break;
            }
        }
    }
    let mut candidate = String::with_capacity(length + 8);
    if negative {
        candidate.push('-');
    }
    candidate.push(even[0] as char);
    if length > 1 {
        candidate.push('.');
        candidate.extend(even[1..].iter().map(|&d| d as char));
    }
    let _ = write!(candidate, "e{exponent}");
    if round_trips(&candidate) {
        candidate
    } else {
        scientific
    }
}

/// |x| = odd * 2^exponent for a nonzero finite float's magnitude bits.
fn odd_parts(bits: u64, mantissa_bits: u32, bias: i32) -> (u64, i32) {
    let field = (bits >> mantissa_bits) as i32;
    let fraction = bits & ((1 << mantissa_bits) - 1);
    let (mut m, mut e) = if field == 0 {
        (fraction, 1 - bias)
    } else {
        (fraction | 1 << mantissa_bits, field - bias)
    };
    let zeros = m.trailing_zeros();
    m >>= zeros;
    e += zeros as i32;
    (m, e)
}

/// Whether the exact value could lie halfway between two `length`-digit
/// decimals: its exact expansion must then end at digit `length + 1`. With
/// x = odd * 2^e and e < 0 the expansion ends exactly at the 10^e place, so
/// it has `exponent - e + 1` significant digits; integers below 2^precision
/// are printed exactly and cannot tie; larger ones are checked in full.
fn may_tie(odd: u64, e: i32, precision: u32, exponent: i32, length: usize) -> bool {
    if e < 0 {
        exponent - e + 1 == length as i32 + 1
    } else {
        e + (64 - odd.leading_zeros() as i32) > precision as i32
    }
}

/// Shortest digits of a nonzero finite float as ryu prints them: Rust's
/// `{:e}` unless a tie is possible, then the tie resolved to even.
#[inline]
fn write_shortest(
    scientific: Scientific,
    may_tie: impl Fn(i32, usize) -> bool,
    exact: impl Fn(usize) -> String,
    round_trips: impl Fn(&str) -> bool,
    max: i32,
    min: i32,
    out: &mut Out,
) {
    let (negative, digits, length, exponent) = scientific.parts();
    if !may_tie(exponent, length) {
        write_float(negative, &digits[..length], exponent, max, min, out);
        return;
    }
    let text = std::str::from_utf8(&scientific.buf[..scientific.len]).expect("ASCII");
    let resolved = ryu_digits(text.to_owned(), may_tie, exact, round_trips);
    let resolved = Scientific::new(format_args!("{resolved}"));
    let (negative, digits, length, exponent) = resolved.parts();
    write_float(negative, &digits[..length], exponent, max, min, out);
}

pub fn write_f64(f: f64, out: &mut Out) {
    if !f.is_finite() {
        out.extend_from_slice(b"null");
    } else if f == 0.0 {
        out.extend_from_slice(if f.is_sign_negative() {
            b"-0.0"
        } else {
            b"0.0"
        });
    } else {
        let (mantissa, binary_exponent) = odd_parts(f.to_bits() & !(1 << 63), 52, 1075);
        write_shortest(
            Scientific::new(format_args!("{f:e}")),
            |exponent, length| may_tie(mantissa, binary_exponent, 53, exponent, length),
            |n| format!("{f:.n$e}"),
            |s| s.parse::<f64>() == Ok(f),
            16,
            -5,
            out,
        );
    }
}
pub fn write_f32(f: f32, out: &mut Out) {
    if !f.is_finite() {
        out.extend_from_slice(b"null");
    } else if f == 0.0 {
        out.extend_from_slice(if f.is_sign_negative() {
            b"-0.0"
        } else {
            b"0.0"
        });
    } else {
        let (mantissa, binary_exponent) = odd_parts((f.to_bits() & !(1 << 31)) as u64, 23, 150);
        write_shortest(
            Scientific::new(format_args!("{f:e}")),
            |exponent, length| may_tie(mantissa, binary_exponent, 24, exponent, length),
            |n| format!("{f:.n$e}"),
            |s| s.parse::<f32>() == Ok(f),
            13,
            -6,
            out,
        );
    }
}

impl ToJson for Value {
    fn write_json(&self, out: &mut Out) {
        match self {
            Value::Null => out.extend_from_slice(b"null"),
            Value::Bool(b) => b.write_json(out),
            Value::Number(Number(N::PosInt(n))) => n.write_json(out),
            Value::Number(Number(N::NegInt(n))) => n.write_json(out),
            Value::Number(Number(N::Float(f))) => write_f64(*f, out),
            Value::String(s) => write_str(s, out),
            Value::Array(items) => items.write_json(out),
            Value::Object(map) => map.write_json(out),
        }
    }
    fn to_value(&self) -> Value {
        self.clone()
    }
}
impl ToJson for bool {
    fn write_json(&self, out: &mut Out) {
        out.extend_from_slice(if *self { b"true" } else { b"false" });
    }
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }
}
const PAIRS: &[u8; 200] = b"0001020304050607080910111213141516171819\
2021222324252627282930313233343536373839\
4041424344454647484950515253545556575859\
6061626364656667686970717273747576777879\
8081828384858687888990919293949596979899";

/// Decimal digits of `n` (preceded by '-' when `negative`).
#[inline]
fn write_integer(negative: bool, mut n: u64, out: &mut Out) {
    let mut buf = [0u8; 21];
    let mut i = buf.len();
    while n >= 1 << 32 {
        let pair = (n % 100) as usize * 2;
        n /= 100;
        i -= 2;
        buf[i..i + 2].copy_from_slice(&PAIRS[pair..pair + 2]);
    }
    let mut n = n as u32;
    while n >= 100 {
        let pair = (n % 100) as usize * 2;
        n /= 100;
        i -= 2;
        buf[i..i + 2].copy_from_slice(&PAIRS[pair..pair + 2]);
    }
    if n >= 10 {
        i -= 2;
        buf[i..i + 2].copy_from_slice(&PAIRS[n as usize * 2..n as usize * 2 + 2]);
    } else {
        i -= 1;
        buf[i] = b'0' + n as u8;
    }
    if negative {
        i -= 1;
        buf[i] = b'-';
    }
    out.extend_from_slice(&buf[i..]);
}
macro_rules! unsigned {
    ($($t:ty),*) => {$(
        impl ToJson for $t {
            fn write_json(&self, out: &mut Out) {
                write_integer(false, *self as u64, out);
            }
            fn to_value(&self) -> Value {
                Value::Number(Number(N::PosInt(*self as u64)))
            }
        }
    )*};
}
unsigned!(u8, u16, u32, u64, usize);
macro_rules! signed {
    ($($t:ty),*) => {$(
        impl ToJson for $t {
            fn write_json(&self, out: &mut Out) {
                write_integer(*self < 0, (*self as i64).unsigned_abs(), out);
            }
            fn to_value(&self) -> Value {
                let n = *self as i64;
                Value::Number(Number(if n < 0 { N::NegInt(n) } else { N::PosInt(n as u64) }))
            }
        }
    )*};
}
signed!(i8, i16, i32, i64, isize);
impl ToJson for f64 {
    fn write_json(&self, out: &mut Out) {
        write_f64(*self, out);
    }
    fn to_value(&self) -> Value {
        Number::from_f64(*self).map_or(Value::Null, Value::Number)
    }
}
impl ToJson for f32 {
    fn write_json(&self, out: &mut Out) {
        write_f32(*self, out);
    }
    fn to_value(&self) -> Value {
        Number::from_f64(*self as f64).map_or(Value::Null, Value::Number)
    }
}
impl ToJson for str {
    fn write_json(&self, out: &mut Out) {
        write_str(self, out);
    }
    fn to_value(&self) -> Value {
        Value::String(self.to_owned())
    }
}
impl ToJson for String {
    fn write_json(&self, out: &mut Out) {
        write_str(self, out);
    }
    fn to_value(&self) -> Value {
        Value::String(self.clone())
    }
}
impl ToJson for std::borrow::Cow<'_, str> {
    fn write_json(&self, out: &mut Out) {
        write_str(self, out);
    }
    fn to_value(&self) -> Value {
        Value::String(self.to_string())
    }
}
impl<T: ToJson + ?Sized> ToJson for &T {
    fn write_json(&self, out: &mut Out) {
        (**self).write_json(out);
    }
    fn to_value(&self) -> Value {
        (**self).to_value()
    }
}
impl<T: ToJson> ToJson for Option<T> {
    fn write_json(&self, out: &mut Out) {
        match self {
            Some(v) => v.write_json(out),
            None => out.extend_from_slice(b"null"),
        }
    }
    fn to_value(&self) -> Value {
        self.as_ref().map_or(Value::Null, ToJson::to_value)
    }
}
impl<T: ToJson> ToJson for [T] {
    fn write_json(&self, out: &mut Out) {
        out.push(b'[');
        for (i, item) in self.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            item.write_json(out);
            out.spill();
        }
        out.push(b']');
    }
    fn to_value(&self) -> Value {
        Value::Array(self.iter().map(ToJson::to_value).collect())
    }
}
impl<T: ToJson, const LEN: usize> ToJson for [T; LEN] {
    fn write_json(&self, out: &mut Out) {
        self[..].write_json(out);
    }
    fn to_value(&self) -> Value {
        self[..].to_value()
    }
}
impl<T: ToJson> ToJson for Vec<T> {
    fn write_json(&self, out: &mut Out) {
        self[..].write_json(out);
    }
    fn to_value(&self) -> Value {
        self[..].to_value()
    }
}
impl<K: AsRef<str> + Ord, T: ToJson> ToJson for BTreeMap<K, T> {
    fn write_json(&self, out: &mut Out) {
        out.push(b'{');
        for (i, (key, value)) in self.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            write_str(key.as_ref(), out);
            out.push(b':');
            value.write_json(out);
        }
        out.push(b'}');
    }
    fn to_value(&self) -> Value {
        Value::Object(
            self.iter()
                .map(|(k, v)| (k.as_ref().to_owned(), v.to_value()))
                .collect(),
        )
    }
}

/// Writes a struct's members in order: `{"a":..,"b":..}`.
pub struct ObjectWriter<'a, 'b> {
    out: &'a mut Out<'b>,
    first: bool,
}
impl<'a, 'b> ObjectWriter<'a, 'b> {
    pub fn new(out: &'a mut Out<'b>) -> Self {
        out.push(b'{');
        Self { out, first: true }
    }
    pub fn field<T: ToJson + ?Sized>(&mut self, key: &str, value: &T) -> &mut Self {
        if !self.first {
            self.out.push(b',');
        }
        self.first = false;
        write_str(key, self.out);
        self.out.push(b':');
        value.write_json(self.out);
        self
    }
    pub fn end(&mut self) {
        self.out.push(b'}');
    }
}

/// `ToJson` for a struct with named members, written in the order given;
/// members written `name: skip_none` are omitted when `None` (serde's
/// `skip_serializing_if = "Option::is_none"`).
#[macro_export]
macro_rules! json_struct {
    ($type:ty { $($field:ident $(: $skip:ident)?),* $(,)? }) => {
        impl $crate::json::ToJson for $type {
            fn write_json(&self, out: &mut $crate::json::Out) {
                let mut object = $crate::json::ObjectWriter::new(out);
                $( $crate::json_struct_field!(write object self $field $($skip)?); )*
                object.end();
            }
            fn to_value(&self) -> $crate::json::Value {
                let mut map = $crate::json::Map::new();
                $( $crate::json_struct_field!(value map self $field $($skip)?); )*
                $crate::json::Value::Object(map)
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! json_struct_field {
    (write $object:ident $self:ident $field:ident) => {
        $object.field(stringify!($field), &$self.$field);
    };
    (write $object:ident $self:ident $field:ident skip_none) => {
        if let Some(value) = &$self.$field {
            $object.field(stringify!($field), value);
        }
    };
    (value $map:ident $self:ident $field:ident) => {
        $map.insert(
            stringify!($field).to_owned(),
            $crate::json::ToJson::to_value(&$self.$field),
        );
    };
    (value $map:ident $self:ident $field:ident skip_none) => {
        if let Some(value) = &$self.$field {
            $map.insert(
                stringify!($field).to_owned(),
                $crate::json::ToJson::to_value(value),
            );
        }
    };
}

/// `ToJson` for a field-less enum written as a string per variant.
#[macro_export]
macro_rules! json_enum {
    ($type:ty { $($variant:ident => $name:literal),* $(,)? }) => {
        impl $crate::json::ToJson for $type {
            fn write_json(&self, out: &mut $crate::json::Out) {
                $crate::json::write_str(match self { $(Self::$variant => $name),* }, out);
            }
            fn to_value(&self) -> $crate::json::Value {
                $crate::json::Value::String(match self { $(Self::$variant => $name),* }.to_owned())
            }
        }
    };
}

/// A [`Value`] from JSON-like syntax: `json!({"a": 1, "b": [x, null]})`.
/// Object and array members are nested objects or arrays, `null`, or any
/// expression implementing `ToJson`; object keys are string literals or
/// parenthesized expressions.
#[macro_export]
macro_rules! json {
    (null) => { $crate::json::Value::Null };
    ([ $($tt:tt)* ]) => {{
        #[allow(unused_mut)]
        let mut array: Vec<$crate::json::Value> = Vec::new();
        $crate::json_items!(array $($tt)*);
        $crate::json::Value::Array(array)
    }};
    ({ $($tt:tt)* }) => {{
        #[allow(unused_mut)]
        let mut map = $crate::json::Map::new();
        $crate::json_members!(map $($tt)*);
        $crate::json::Value::Object(map)
    }};
    ($other:expr) => { $crate::json::to_value(&$other) };
}

#[doc(hidden)]
#[macro_export]
macro_rules! json_items {
    ($array:ident) => {};
    ($array:ident null $(, $($rest:tt)*)?) => {
        $array.push($crate::json::Value::Null);
        $crate::json_items!($array $($($rest)*)?);
    };
    ($array:ident [ $($inner:tt)* ] $(, $($rest:tt)*)?) => {
        $array.push($crate::json!([ $($inner)* ]));
        $crate::json_items!($array $($($rest)*)?);
    };
    ($array:ident { $($inner:tt)* } $(, $($rest:tt)*)?) => {
        $array.push($crate::json!({ $($inner)* }));
        $crate::json_items!($array $($($rest)*)?);
    };
    ($array:ident $value:expr $(, $($rest:tt)*)?) => {
        $array.push($crate::json::to_value(&$value));
        $crate::json_items!($array $($($rest)*)?);
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! json_members {
    ($map:ident) => {};
    ($map:ident $key:literal : $($rest:tt)*) => {
        $crate::json_member!($map ($key) $($rest)*);
    };
    ($map:ident ($key:expr) : $($rest:tt)*) => {
        $crate::json_member!($map ($key) $($rest)*);
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! json_member {
    ($map:ident ($key:expr) null $(, $($rest:tt)*)?) => {
        $map.insert(::std::string::String::from($key), $crate::json::Value::Null);
        $crate::json_members!($map $($($rest)*)?);
    };
    ($map:ident ($key:expr) [ $($inner:tt)* ] $(, $($rest:tt)*)?) => {
        $map.insert(::std::string::String::from($key), $crate::json!([ $($inner)* ]));
        $crate::json_members!($map $($($rest)*)?);
    };
    ($map:ident ($key:expr) { $($inner:tt)* } $(, $($rest:tt)*)?) => {
        $map.insert(::std::string::String::from($key), $crate::json!({ $($inner)* }));
        $crate::json_members!($map $($($rest)*)?);
    };
    ($map:ident ($key:expr) $value:expr $(, $($rest:tt)*)?) => {
        $map.insert(::std::string::String::from($key), $crate::json::to_value(&$value));
        $crate::json_members!($map $($($rest)*)?);
    };
}

/// Parse JSON text (RFC 8259) into a [`Value`].
pub fn from_slice(text: &[u8]) -> Result<Value, ParseError> {
    let mut p = Parser { s: text, i: 0 };
    p.ws();
    let value = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.error("trailing characters"));
    }
    Ok(value)
}
pub fn from_str(text: &str) -> Result<Value, ParseError> {
    from_slice(text.as_bytes())
}

#[derive(Debug)]
pub struct ParseError {
    message: &'static str,
    offset: usize,
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON {} at byte {}", self.message, self.offset)
    }
}
impl std::error::Error for ParseError {}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}
impl Parser<'_> {
    fn error(&self, message: &'static str) -> ParseError {
        ParseError {
            message,
            offset: self.i,
        }
    }
    fn ws(&mut self) {
        while matches!(self.s.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }
    fn eat(&mut self, literal: &[u8]) -> Result<(), ParseError> {
        if self.s[self.i..].starts_with(literal) {
            self.i += literal.len();
            Ok(())
        } else {
            Err(self.error("invalid literal"))
        }
    }
    fn value(&mut self, depth: usize) -> Result<Value, ParseError> {
        if depth > 128 {
            return Err(self.error("nesting too deep"));
        }
        match self.s.get(self.i) {
            Some(b'n') => self.eat(b"null").map(|_| Value::Null),
            Some(b't') => self.eat(b"true").map(|_| Value::Bool(true)),
            Some(b'f') => self.eat(b"false").map(|_| Value::Bool(false)),
            Some(b'"') => self.string().map(Value::String),
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Value::Array(items));
                }
                loop {
                    self.ws();
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::Array(items));
                        }
                        _ => return Err(self.error("expected , or ]")),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut map = Map::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Value::Object(map));
                }
                loop {
                    self.ws();
                    if self.s.get(self.i) != Some(&b'"') {
                        return Err(self.error("expected key"));
                    }
                    let key = self.string()?;
                    self.ws();
                    if self.s.get(self.i) != Some(&b':') {
                        return Err(self.error("expected :"));
                    }
                    self.i += 1;
                    self.ws();
                    let value = self.value(depth + 1)?;
                    map.insert(key, value);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Object(map));
                        }
                        _ => return Err(self.error("expected , or }")),
                    }
                }
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.error("expected value")),
        }
    }
    fn number(&mut self) -> Result<Value, ParseError> {
        let start = self.i;
        let negative = self.s[self.i] == b'-';
        if negative {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let from = p.i;
            while matches!(p.s.get(p.i), Some(b'0'..=b'9')) {
                p.i += 1;
            }
            p.i - from
        };
        let int_start = self.i;
        let n = digits(self);
        if n == 0 || (n > 1 && self.s[int_start] == b'0') {
            return Err(self.error("invalid number"));
        }
        let mut float = false;
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            if digits(self) == 0 {
                return Err(self.error("invalid number"));
            }
            float = true;
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if digits(self) == 0 {
                return Err(self.error("invalid number"));
            }
            float = true;
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).expect("ASCII");
        if !float {
            if negative {
                // "-0" is the float -0.0, as serde_json reads it.
                if let Ok(v @ ..0) = text.parse::<i64>() {
                    return Ok(Value::Number(Number(N::NegInt(v))));
                }
            } else if let Ok(v) = text.parse::<u64>() {
                return Ok(Value::Number(Number(N::PosInt(v))));
            }
        }
        let f: f64 = text.parse().map_err(|_| self.error("invalid number"))?;
        Number::from_f64(f)
            .map(Value::Number)
            .ok_or_else(|| self.error("number out of range"))
    }
    fn hex4(&mut self) -> Result<u32, ParseError> {
        let digits = self
            .s
            .get(self.i..self.i + 4)
            .ok_or_else(|| self.error("invalid escape"))?;
        let text = std::str::from_utf8(digits).map_err(|_| self.error("invalid escape"))?;
        let v = u32::from_str_radix(text, 16).map_err(|_| self.error("invalid escape"))?;
        if !digits.iter().all(u8::is_ascii_hexdigit) {
            return Err(self.error("invalid escape"));
        }
        self.i += 4;
        Ok(v)
    }
    fn string(&mut self) -> Result<String, ParseError> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let b = *self
                .s
                .get(self.i)
                .ok_or_else(|| self.error("unterminated string"))?;
            self.i += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let e = *self
                        .s
                        .get(self.i)
                        .ok_or_else(|| self.error("invalid escape"))?;
                    self.i += 1;
                    match e {
                        b'"' | b'\\' | b'/' => out.push(e),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut c = self.hex4()?;
                            if (0xd800..0xdc00).contains(&c) {
                                self.eat(b"\\u").map_err(|_| self.error("lone surrogate"))?;
                                let low = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&low) {
                                    return Err(self.error("lone surrogate"));
                                }
                                c = 0x10000 + ((c - 0xd800) << 10) + (low - 0xdc00);
                            }
                            let ch =
                                char::from_u32(c).ok_or_else(|| self.error("lone surrogate"))?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err(self.error("invalid escape")),
                    }
                }
                0..=0x1f => return Err(self.error("control character in string")),
                _ => out.push(b),
            }
        }
        String::from_utf8(out).map_err(|_| self.error("invalid UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json_reference as reference;

    fn xorshift(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    #[test]
    fn floats_match_serde_json() {
        let mut specials: Vec<f64> = vec![
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.1,
            0.3,
            1.5,
            100.0,
            1e15,
            1e16,
            1e17,
            9007199254740993.0,
            1e-5,
            1e-6,
            1e-7,
            0.0001234,
            123456789012345680.0,
            5e-324,
            f64::MIN_POSITIVE,
            f64::MAX,
            f64::MIN,
            f64::EPSILON,
            2.0f64.powi(53),
            1.0 / 3.0,
            -70.0,
            -0.691,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        for e in -30..30 {
            specials.push(10f64.powi(e));
            specials.push(1.25 * 10f64.powi(e));
        }
        let mut seed = 0x1234_5678_9abc_def0u64;
        let random = (0..1_000_000).map(|_| f64::from_bits(xorshift(&mut seed)));
        for f in specials.into_iter().chain(random) {
            let mut ours = Out::new();
            write_f64(f, &mut ours);
            assert_eq!(
                String::from_utf8(ours.into_bytes()).unwrap(),
                reference::to_string(&f).unwrap(),
                "{f:e}"
            );
        }
        let mut seed = 0x0fed_cba9_8765_4321u64;
        let random = (0..1_000_000).map(|_| f32::from_bits(xorshift(&mut seed) as u32));
        let specials = [
            0.0f32,
            -0.0,
            1.0,
            0.1,
            1e-6,
            1e-7,
            1e12,
            1e13,
            1e14,
            f32::MAX,
            f32::MIN_POSITIVE,
            1e-45,
        ];
        for f in specials.into_iter().chain(random) {
            let mut ours = Out::new();
            write_f32(f, &mut ours);
            assert_eq!(
                String::from_utf8(ours.into_bytes()).unwrap(),
                reference::to_string(&f).unwrap(),
                "{f:e}"
            );
            // f32 inside a Value is widened to f64, as serde_json does.
            assert_eq!(
                to_string(&f.to_value()),
                reference::to_value(f).unwrap().to_string(),
                "{f:e}"
            );
        }
    }

    #[test]
    fn strings_integers_and_values_match_serde_json() {
        let mut text = String::new();
        for c in (0u32..0x300).chain([0x2028, 0x2029, 0xfeff, 0x1f600]) {
            text.push(char::from_u32(c).unwrap());
        }
        for s in [
            text.as_str(),
            "",
            "a\"b\\c/d\u{7f}",
            "tab\tnew\nline\r\u{8}\u{c}",
        ] {
            assert_eq!(to_string(s), reference::to_string(s).unwrap());
        }
        for n in [0i64, 1, -1, i64::MIN, i64::MAX] {
            assert_eq!(to_string(&n), reference::to_string(&n).unwrap());
        }
        assert_eq!(
            to_string(&u64::MAX),
            reference::to_string(&u64::MAX).unwrap()
        );
        let tags: BTreeMap<String, String> =
            [("b".into(), "x".into()), ("a".into(), "y\n".into())].into();
        let x = 1.5f64;
        let ours = crate::json!({"z": 1, "a": [1, null, {"k": x}, [], {}], "m": tags, "f": -0.25f32,
            "t": true, "s": if x > 1.0 {"big"} else {"small"}, "n": null, "o": Some(3u8), "none": None::<u8>});
        let theirs = reference::json!({"z": 1, "a": [1, null, {"k": x}, [], {}], "m": tags, "f": -0.25f32,
            "t": true, "s": if x > 1.0 {"big"} else {"small"}, "n": null, "o": Some(3u8), "none": None::<u8>});
        assert_eq!(ours.to_string(), theirs.to_string());
        assert_eq!(
            to_string_pretty(&ours),
            reference::to_string_pretty(&theirs).unwrap()
        );
        assert_eq!(to_string_pretty(&json!([])), "[]");
    }

    #[test]
    fn parser_round_trips_what_serde_json_reads() {
        let texts = [
            r#"{"a":[1,-2,3.5,-0.0,1e300,-1e-400,18446744073709551615,18446744073709551616,-9223372036854775808,-9223372036854775809],"b":{"c":null,"d":true,"e":false},"s":"\u00e9\ud83d\ude00\"\\\/\b\f\n\r\t"}"#,
            " [ 1 , { \"k\" : [ ] } , \"x\" ] ",
            "0",
            "-0",
            "\"\"",
            r#"{"dup":1,"dup":2}"#,
        ];
        for text in texts {
            let theirs: reference::Value = reference::from_str(text).unwrap();
            match from_str(text) {
                Ok(ours) => assert_eq!(to_string(&ours), theirs.to_string(), "{text}"),
                Err(_) => panic!("{text}"),
            }
        }
        for bad in [
            "1e400",
            "",
            "[1,]",
            "{\"a\" 1}",
            "01",
            "1.",
            "-",
            "\"\\x\"",
            "\"\\ud800\"",
            "[1] 2",
            "nul",
            "\"a\nb\"",
        ] {
            assert!(from_str(bad).is_err(), "{bad}");
            assert!(
                reference::from_str::<reference::Value>(bad).is_err(),
                "{bad}"
            );
        }
    }
}
