use serde_json::Value;
use std::{
    fmt::Write as _,
    io::{self, BufRead},
};

/// Maximum bytes per input record, excluding the newline delimiter.
pub const MAX_LINE_BYTES: usize = 64 * 1024;
pub const UNKNOWN_UNIT: &str = "(unknown)";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    pub realtime_us: Option<u64>,
    pub cursor: Option<String>,
    pub priority: Option<u8>,
    pub host: Option<String>,
    pub unit: String,
    pub unit_metadata: UnitMetadata,
    pub message: String,
}

impl Event {
    /// Resolved unit's source field followed by the other available journal fields.
    pub fn unit_fields(&self) -> impl Iterator<Item = (UnitField, &str)> {
        self.unit_metadata
            .source
            .map(|source| (source, self.unit.as_str()))
            .into_iter()
            .chain(
                self.unit_metadata
                    .secondary
                    .iter()
                    .map(|(source, value)| (*source, value.as_ref())),
            )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnitField {
    UserUnit,
    SystemUnit,
    SyslogIdentifier,
    Command,
}

impl UnitField {
    const PREFERENCE: [Self; 4] = [
        Self::UserUnit,
        Self::SystemUnit,
        Self::SyslogIdentifier,
        Self::Command,
    ];

    pub fn journal_name(self) -> &'static str {
        match self {
            Self::UserUnit => "_SYSTEMD_USER_UNIT",
            Self::SystemUnit => "_SYSTEMD_UNIT",
            Self::SyslogIdentifier => "SYSLOG_IDENTIFIER",
            Self::Command => "_COMM",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnitMetadata {
    source: Option<UnitField>,
    secondary: Box<[(UnitField, Box<str>)]>,
}

impl UnitMetadata {
    pub fn new(source: UnitField, secondary: Box<[(UnitField, Box<str>)]>) -> Self {
        Self {
            source: Some(source),
            secondary,
        }
    }

    /// Retained heap allocations, excluding the resolved name stored in Event::unit.
    pub fn heap_bytes(&self) -> usize {
        size_of_val(self.secondary.as_ref())
            + self
                .secondary
                .iter()
                .map(|(_, value)| value.len())
                .sum::<usize>()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Frame {
    Event(Event),
    Malformed,
    Oversized,
}

/// A newline-delimited journalctl JSON reader with a bounded record buffer.
pub struct Decoder<R> {
    reader: R,
    line: LineBuffer,
}

impl<R: BufRead> Decoder<R> {
    pub fn new(reader: R, max_line_bytes: usize) -> Self {
        Self {
            reader,
            line: LineBuffer::new(max_line_bytes),
        }
    }

    pub fn next_frame(&mut self) -> io::Result<Option<Frame>> {
        loop {
            let bytes = self.reader.fill_buf()?;
            if bytes.is_empty() {
                return Ok(self.line.is_pending().then(|| self.line.take(record)));
            }
            let (consumed, complete) = self.line.push(bytes);
            self.reader.consume(consumed);
            if complete {
                return Ok(Some(self.line.take(record)));
            }
        }
    }
}

pub(crate) fn record(line: Option<&[u8]>) -> Frame {
    let Some(bytes) = line else {
        return Frame::Oversized;
    };
    let Ok(Value::Object(object)) = serde_json::from_slice::<Value>(bytes) else {
        return Frame::Malformed;
    };
    let text = |key| field(object.get(key), |value| value.text(false));
    let mut unit_fields = UnitField::PREFERENCE
        .into_iter()
        .filter_map(|field| text(field.journal_name()).map(|value| (field, value)));
    let (unit, unit_metadata) = match unit_fields.next() {
        Some((source, unit)) => {
            let secondary = unit_fields
                .map(|(field, value)| (field, value.into_boxed_str()))
                .collect();
            (unit, UnitMetadata::new(source, secondary))
        }
        None => (UNKNOWN_UNIT.to_owned(), UnitMetadata::default()),
    };

    Frame::Event(Event {
        realtime_us: field(object.get("__REALTIME_TIMESTAMP"), |value| value.number()),
        cursor: text("__CURSOR"),
        priority: field(object.get("PRIORITY"), |value| {
            let number = value.number()?;
            (number <= 7).then_some(number as u8)
        }),
        host: text("_HOSTNAME"),
        unit,
        unit_metadata,
        message: field(object.get("MESSAGE"), |value| value.text(true))
            .unwrap_or_else(|| "\\(message unavailable)".to_owned()),
    })
}

enum Field<'a> {
    Text(&'a str),
    Bytes(Vec<u8>),
    Number(u64),
}

impl Field<'_> {
    fn from_value(value: &Value) -> Option<Field<'_>> {
        match value {
            Value::String(text) => Some(Field::Text(text)),
            Value::Array(values) if !values.is_empty() => values
                .iter()
                .map(|value| u8::try_from(value.as_u64()?).ok())
                .collect::<Option<Vec<_>>>()
                .map(Field::Bytes),
            Value::Number(number) => number.as_u64().map(Field::Number),
            _ => None,
        }
    }

    fn number(self) -> Option<u64> {
        match self {
            Field::Text(text) => text.parse().ok(),
            Field::Bytes(bytes) => std::str::from_utf8(&bytes).ok()?.parse().ok(),
            Field::Number(number) => Some(number),
        }
    }

    fn text(self, allow_empty: bool) -> Option<String> {
        let bytes = match &self {
            Field::Text(text) => text.as_bytes(),
            Field::Bytes(bytes) => bytes.as_slice(),
            Field::Number(_) => return None,
        };
        (allow_empty || !bytes.is_empty()).then(|| sanitize_bytes(bytes))
    }
}

fn field<T>(value: Option<&Value>, convert: impl Fn(Field<'_>) -> Option<T>) -> Option<T> {
    let value = value?;
    if let Some(single) = Field::from_value(value) {
        return convert(single);
    }
    let values = value.as_array()?;
    if values.iter().all(Value::is_number) {
        // An all-numeric array represents bytes. Invalid bytes invalidate that
        // value rather than becoming duplicate numeric fields.
        return None;
    }
    // Journal JSON nests a byte array when it appears among duplicate values.
    values
        .iter()
        .filter_map(Field::from_value)
        .find_map(convert)
}

/// Keep journal fields on one printable terminal line. Escaping backslashes
/// makes a displayed escape distinguishable from literal text of that form.
pub fn sanitize(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    append_safe(text, &mut output);
    output
}

fn sanitize_bytes(mut bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len());
    loop {
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                append_safe(text, &mut output);
                return output;
            }
            Err(error) => {
                let (valid, remaining) = bytes.split_at(error.valid_up_to());
                append_safe(
                    std::str::from_utf8(valid).expect("valid UTF-8 prefix"),
                    &mut output,
                );
                let (invalid, rest) =
                    remaining.split_at(error.error_len().unwrap_or(remaining.len()));
                for byte in invalid {
                    write!(output, "\\x{byte:02x}").expect("writing to String is infallible");
                }
                bytes = rest;
            }
        }
    }
}

fn append_safe(text: &str, output: &mut String) {
    for ch in text.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\0'..='\u{1f}' | '\u{7f}' => {
                write!(output, "\\x{:02x}", u32::from(ch))
                    .expect("writing to String is infallible");
            }
            '\u{061c}'
            | '\u{200e}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => {
                write!(output, "\\u{{{:x}}}", u32::from(ch))
                    .expect("writing to String is infallible");
            }
            ch if ch.is_control() => {
                write!(output, "\\u{{{:x}}}", u32::from(ch))
                    .expect("writing to String is infallible");
            }
            ch => output.push(ch),
        }
    }
}

/// Assemble at most one line while draining the remainder of oversized input.
pub(crate) struct LineBuffer {
    bytes: Vec<u8>,
    limit: usize,
    oversized: bool,
}

impl LineBuffer {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            oversized: false,
        }
    }

    /// Return bytes consumed and whether they finished a line.
    pub(crate) fn push(&mut self, input: &[u8]) -> (usize, bool) {
        let newline = input.iter().position(|&byte| byte == b'\n');
        let end = newline.unwrap_or(input.len());

        if !self.oversized {
            if end > self.limit - self.bytes.len() {
                self.bytes.clear();
                self.oversized = true;
            } else {
                let needed = self.bytes.len() + end;
                if needed > self.bytes.capacity() {
                    let capacity = self.bytes.capacity().saturating_mul(2);
                    let reserve = needed.max(capacity).min(self.limit);
                    self.bytes.reserve_exact(reserve - self.bytes.len());
                }
                self.bytes.extend_from_slice(&input[..end]);
            }
        }

        (end + usize::from(newline.is_some()), newline.is_some())
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.oversized || !self.bytes.is_empty()
    }

    pub(crate) fn take<T>(&mut self, convert: impl FnOnce(Option<&[u8]>) -> T) -> T {
        let result = convert((!self.oversized).then_some(&self.bytes));
        self.bytes.clear();
        self.oversized = false;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn duplicates_use_first_usable_value_and_binary_is_reversible() {
        let input = br#"{"_SYSTEMD_USER_UNIT":["",null,"user.service"],"_SYSTEMD_UNIT":"system.service","_HOSTNAME":[[104,111,115,116],"other"],"PRIORITY":["9","4"],"__REALTIME_TIMESTAMP":["bad","42"],"__CURSOR":"cursor","MESSAGE":[null,[27,128,194,128,92,10],"ignored"]}"#;
        let Frame::Event(event) = record(Some(input)) else {
            panic!("valid journal JSON was rejected");
        };
        assert_eq!(event.unit, "user.service");
        assert_eq!(event.host.as_deref(), Some("host"));
        assert_eq!(event.priority, Some(4));
        assert_eq!(event.realtime_us, Some(42));
        assert_eq!(event.cursor.as_deref(), Some("cursor"));
        assert_eq!(event.message, r"\x1b\x80\u{80}\\\n");
        assert_ne!(event.message, sanitize(&event.message));
    }

    #[test]
    fn decoder_drains_an_oversized_line_and_preserves_the_next_frame() {
        let input = b"0123456789\n{}\n[]\n{}";
        let mut decoder = Decoder::new(BufReader::with_capacity(2, &input[..]), 3);
        assert_eq!(decoder.next_frame().unwrap(), Some(Frame::Oversized));
        assert!(matches!(
            decoder.next_frame().unwrap(),
            Some(Frame::Event(_))
        ));
        assert_eq!(decoder.next_frame().unwrap(), Some(Frame::Malformed));
        assert!(matches!(
            decoder.next_frame().unwrap(),
            Some(Frame::Event(_))
        ));
        assert_eq!(decoder.next_frame().unwrap(), None);
        assert!(decoder.line.bytes.capacity() <= 3);
    }

    #[test]
    fn full_and_empty_lines_at_eof() {
        let mut decoder = Decoder::new(&b"\n{}\n123"[..], 2);
        assert_eq!(decoder.next_frame().unwrap(), Some(Frame::Malformed));
        assert!(matches!(
            decoder.next_frame().unwrap(),
            Some(Frame::Event(_))
        ));
        assert_eq!(decoder.next_frame().unwrap(), Some(Frame::Oversized));
        assert_eq!(decoder.next_frame().unwrap(), None);
    }
}
