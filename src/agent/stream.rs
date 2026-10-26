//! Follows the LLM's JSON answer while it streams in, so speech can start
//! before the answer is complete: root-level members are collected as soon as
//! each is complete, then the `text` string is decoded piece by piece (JSON
//! escapes may be split between pieces) and cut into speakable chunks.

use serde_json::{Map, Value};

/// What one piece of the answer revealed about the `text` member
#[derive(Debug, Default, PartialEq)]
pub struct Scanned {
    /// The `text` string began; the members written before it are in `fields`
    pub text_started: bool,
    /// Decoded characters of the `text` string
    pub text: String,
    /// The `text` string ended
    pub text_ended: bool,
}

/// The root object of the answer, scanned character by character
#[derive(Default)]
pub struct AnswerScanner {
    state: State,
    /// Complete root-level members other than the streamed `text`
    pub fields: Map<String, Value>,
    text_seen: bool,
}

#[derive(Default)]
enum State {
    /// Before the root object: whitespace, a markdown fence
    #[default]
    Before,
    /// At the root level, expecting a member name
    Name,
    /// Inside a member name
    InName(StringDecoder, String),
    /// After a member name
    Colon(String),
    /// Before a member value
    Value(String),
    /// Inside a member value other than the text string, kept as raw JSON
    InValue { name: String, raw: String, depth: u32, in_string: bool, escaped: bool },
    /// Inside the text string
    InText(StringDecoder),
    /// After a member value
    Separator,
    /// After the root object
    Done,
}

impl AnswerScanner {
    pub fn push(&mut self, piece: &str) -> Scanned {
        let mut scanned = Scanned::default();
        for character in piece.chars() {
            self.push_char(character, &mut scanned);
        }
        scanned
    }

    fn push_char(&mut self, character: char, scanned: &mut Scanned) {
        self.state = match std::mem::take(&mut self.state) {
            State::Before if character == '{' => State::Name,
            State::Before => State::Before,
            State::Done => State::Done,
            State::Name | State::Separator if character == '}' => State::Done,
            State::Name if character == '"' => State::InName(StringDecoder::default(), String::new()),
            // whitespace and commas between members; anything else is not
            // valid JSON, which the full parse reports
            State::Name | State::Separator if character == ',' => State::Name,
            state @ (State::Name | State::Separator) => state,
            State::InName(mut decoder, mut name) => match decoder.push(character, &mut name) {
                true => State::Colon(name),
                false => State::InName(decoder, name),
            },
            State::Colon(name) if character == ':' => State::Value(name),
            State::Colon(name) => State::Colon(name),
            State::Value(name) if character.is_whitespace() => State::Value(name),
            State::Value(name) if name == "text" && character == '"' && !self.text_seen => {
                self.text_seen = true;
                scanned.text_started = true;
                State::InText(StringDecoder::default())
            }
            State::Value(name) => State::InValue {
                name,
                raw: character.to_string(),
                depth: u32::from(matches!(character, '{' | '[')),
                in_string: character == '"',
                escaped: false,
            },
            State::InValue { name, raw, depth: 0, in_string: false, .. } if matches!(character, ',' | '}') => {
                if let Ok(value) = serde_json::from_str(raw.trim()) {
                    self.fields.insert(name, value);
                }
                if character == ',' { State::Name } else { State::Done }
            }
            State::InValue { name, mut raw, mut depth, mut in_string, mut escaped } => {
                raw.push(character);
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if character == '\\' {
                        escaped = true;
                    } else if character == '"' {
                        in_string = false;
                    }
                } else {
                    match character {
                        '"' => in_string = true,
                        '{' | '[' => depth += 1,
                        '}' | ']' => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                }
                State::InValue { name, raw, depth, in_string, escaped }
            }
            State::InText(mut decoder) => match decoder.push(character, &mut scanned.text) {
                true => {
                    scanned.text_ended = true;
                    State::Separator
                }
                false => State::InText(decoder),
            },
        };
    }
}

/// Decodes a JSON string one character at a time (after its opening quote)
#[derive(Default)]
struct StringDecoder {
    escape: Escape,
    /// A high surrogate waiting for its low half (`😀`)
    high_surrogate: Option<u16>,
}

#[derive(Default)]
enum Escape {
    #[default]
    None,
    Backslash,
    Unicode {
        value: u32,
        digits: u8,
    },
}

impl StringDecoder {
    /// Appends what `character` decodes to; true at the closing quote
    fn push(&mut self, character: char, out: &mut String) -> bool {
        match std::mem::take(&mut self.escape) {
            Escape::None => match character {
                '"' => {
                    self.flush_surrogate(out);
                    return true;
                }
                '\\' => self.escape = Escape::Backslash,
                _ => {
                    self.flush_surrogate(out);
                    out.push(character);
                }
            },
            Escape::Backslash => {
                let decoded = match character {
                    'u' => {
                        self.escape = Escape::Unicode { value: 0, digits: 0 };
                        return false;
                    }
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    // \" \\ \/ and invalid escapes alike: the character itself
                    other => other,
                };
                self.flush_surrogate(out);
                out.push(decoded);
            }
            Escape::Unicode { value, digits } => match character.to_digit(16) {
                Some(digit) if digits < 3 => {
                    self.escape = Escape::Unicode { value: value * 16 + digit, digits: digits + 1 }
                }
                // four digits: one UTF-16 code unit
                Some(digit) => self.unit((value * 16 + digit) as u16, out),
                None => {
                    self.flush_surrogate(out);
                    out.push(char::REPLACEMENT_CHARACTER);
                    return self.push(character, out);
                }
            },
        }
        false
    }

    fn unit(&mut self, unit: u16, out: &mut String) {
        match unit {
            0xD800..=0xDBFF => {
                self.flush_surrogate(out);
                self.high_surrogate = Some(unit);
            }
            0xDC00..=0xDFFF => {
                let decoded = self
                    .high_surrogate
                    .take()
                    .and_then(|high| char::decode_utf16([high, unit]).next())
                    .and_then(Result::ok);
                out.push(decoded.unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            _ => {
                self.flush_surrogate(out);
                out.push(char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER));
            }
        }
    }

    /// A high surrogate not followed by a low one stands for nothing valid
    fn flush_surrogate(&mut self, out: &mut String) {
        if self.high_surrogate.take().is_some() {
            out.push(char::REPLACEMENT_CHARACTER);
        }
    }
}

/// Short lead-ins like «Конечно,» are not worth a request of their own: a cut
/// at a comma, colon or semicolon needs this many new characters (TS fork)
const MIN_CLAUSE_CHARS: usize = 30;

/// Cuts a growing text into speakable chunks, the rule of the TS fork's
/// `findPartialCut`: after the last `. ! ? … \n`, otherwise after the last
/// `, : ;` once enough new text piled up; never inside a number like 3.5 or 12:30
#[derive(Default)]
pub struct TextChunker {
    text: String,
    emitted: usize,
}

impl TextChunker {
    /// Adds decoded text; returns a chunk once one can be cut
    pub fn push(&mut self, piece: &str) -> Option<String> {
        self.text.push_str(piece);
        let cut = self.cut();
        (cut > self.emitted).then(|| self.take(cut))
    }

    /// The text after the last chunk, once the text is complete
    pub fn finish(&mut self) -> Option<String> {
        (self.text.len() > self.emitted).then(|| self.take(self.text.len()))
    }

    fn take(&mut self, cut: usize) -> String {
        let chunk = self.text[self.emitted..cut].to_string();
        self.emitted = cut;
        chunk
    }

    fn cut(&self) -> usize {
        let pending: Vec<(usize, char)> = self.text[self.emitted..]
            .char_indices()
            .map(|(offset, character)| (self.emitted + offset, character))
            .collect();
        let (mut sentence, mut clause) = (None, None);
        for (index, &(offset, character)) in pending.iter().enumerate() {
            let end = offset + character.len_utf8();
            let previous = match index {
                0 => self.text[..self.emitted].chars().next_back(),
                _ => Some(pending[index - 1].1),
            };
            let next = pending.get(index + 1).map(|&(_, character)| character);
            match character {
                '.' | ',' | ':' | ';' if in_number(previous, next) => {}
                '.' | '!' | '?' | '…' | '\n' => sentence = Some(end),
                ',' | ':' | ';' if self.text[self.emitted..end].chars().count() >= MIN_CLAUSE_CHARS => {
                    clause = Some(end)
                }
                _ => {}
            }
        }
        sentence.or(clause).unwrap_or(self.emitted)
    }
}

/// A separator between digits; after a digit with the next character still
/// unwritten it may become one, so the cut waits
fn in_number(previous: Option<char>, next: Option<char>) -> bool {
    previous.is_some_and(|character| character.is_ascii_digit())
        && next.is_none_or(|character| character.is_ascii_digit())
}
