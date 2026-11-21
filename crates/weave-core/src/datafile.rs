//! Structured data files — JSON, TOML, YAML — read the way their consumers
//! read them: as tables of keys.
//!
//! A line merge of a data file can be clean line by line and still be invalid
//! data: two sides each add `tower-http = …` to `[dependencies]` at different
//! lines, both lines survive, and the file no longer loads. No line is stated
//! twice, so the line rulers never see it. What is stated twice is a KEY, at
//! one table path — which is the ruler here.
//!
//! [`keys`] answers, per file, either "this does not parse" or the most times
//! any one table/object states each key. TOML and YAML parsers already reject
//! a key stated twice in a mapping; JSON parsers
//! accept one (the last wins), so JSON is walked here and its keys counted.

use std::collections::HashMap;

mod json5_keys;

/// `"<table path>\u{1f}<key>"` -> the most times one table at that path
/// states the key. JSON array elements share the path `[]`. YAML root
/// sequence IDs have a bare name and a ranked entry for each document
/// containing duplicates, ordered by decreasing multiplicity.
pub type KeyTally = HashMap<String, usize>;

/// The data format of `path`, if it is one weave reads keys out of.
fn format(path: &str) -> Option<&'static str> {
    let (_, ext) = path.rsplit_once('.')?;
    if ext.eq_ignore_ascii_case("json") || ext.eq_ignore_ascii_case("jsonc") {
        Some("JSON")
    } else if ext.eq_ignore_ascii_case("json5") {
        Some("JSON5")
    } else if ext.eq_ignore_ascii_case("jsonl") || ext.eq_ignore_ascii_case("ndjson") {
        Some("JSON Lines")
    } else if ext.eq_ignore_ascii_case("toml") {
        Some("TOML")
    } else if ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml") {
        Some("YAML")
    } else {
        None
    }
}

/// `None` when `path` is not a data file. Otherwise the key tally, or why the
/// text does not parse as its format.
pub fn keys(path: &str, text: &str) -> Option<Result<KeyTally, String>> {
    Some(match format(path)? {
        "JSON" => json_keys(text),
        "JSON5" => json5_keys::parse(text),
        "JSON Lines" => json_lines_keys(text),
        "TOML" => text
            .parse::<toml::Table>()
            .map(|_| KeyTally::new())
            .map_err(|e| e.message().to_string()),
        _ => yaml_keys(text),
    })
}

/// The format's name, for a sentence about it.
pub fn format_name(path: &str) -> Option<&'static str> {
    format(path)
}

pub(crate) fn yaml_documents(
    text: &str,
) -> impl Iterator<Item = Result<oneyaml::Value, oneyaml::Error>> + '_ {
    use serde::Deserialize;
    let settings = oneyaml::Settings::new().honor_version_directive(true);
    oneyaml::Deserializer::from_str_with(text, &settings).map(oneyaml::Value::deserialize)
}

/// Mapping duplicates are load errors. Count sequence duplicates per document
/// so an inherited duplicate cannot mask one added in another document.
fn yaml_keys(text: &str) -> Result<KeyTally, String> {
    use oneyaml::Value;

    let mut keys = KeyTally::new();
    let mut duplicates = HashMap::<String, Vec<usize>>::new();
    let mut local = HashMap::new();
    for doc in yaml_documents(text) {
        let doc = doc.map_err(|e| e.to_string())?;
        let root = doc.untag_ref();
        let mut pending = vec![root];
        while let Some(value) = pending.pop() {
            match value.untag_ref() {
                Value::Mapping(mapping) => {
                    for (key, child) in mapping {
                        if let Value::String(name) = key {
                            keys.insert(format!("\u{1f}{name}"), 1);
                        } else {
                            let name = typed_yaml_key(key).map_err(|e| e.to_string())?;
                            keys.insert(format!("typed\u{1f}{name}"), 1);
                        }
                        pending.push(child);
                    }
                }
                Value::Sequence(items) => pending.extend(items),
                _ => {}
            }
        }
        let Some(items) = root.as_sequence() else {
            continue;
        };
        for item in items {
            // Other fields (e.g. Kubernetes `kind`) can be shared by distinct items.
            let Some(id @ (Value::String(_) | Value::Number(_) | Value::Bool(_))) =
                item.untag_ref().get("id")
            else {
                continue;
            };
            let label = oneyaml::to_string(id).map_err(|e| e.to_string())?;
            *local.entry(label).or_insert(0) += 1;
        }
        for (id, count) in local.drain() {
            if count > 1 {
                duplicates.entry(id.clone()).or_default().push(count);
            }
            keys.insert(id, 1);
        }
    }
    // Ranks survive reordering or adding documents that have no duplicates.
    for (id, mut counts) in duplicates {
        counts.sort_unstable_by(|a, b| b.cmp(a));
        for (index, count) in counts.into_iter().enumerate() {
            keys.insert(format!("{}\u{1f}{id}", index + 1), count);
        }
    }
    Ok(keys)
}

fn typed_yaml_key(key: &oneyaml::Value) -> Result<String, oneyaml::Error> {
    use oneyaml::{TaggedValue, Value};

    fn ordered(value: &Value) -> Result<Value, oneyaml::Error> {
        Ok(match value {
            Value::Mapping(mapping) => {
                let mut entries = Vec::with_capacity(mapping.len());
                for (key, value) in mapping {
                    let key = ordered(key)?;
                    entries.push((oneyaml::to_string(&key)?, key, ordered(value)?));
                }
                entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                Value::Mapping(
                    entries
                        .into_iter()
                        .map(|(_, key, value)| (key, value))
                        .collect(),
                )
            }
            Value::Sequence(items) => {
                Value::Sequence(items.iter().map(ordered).collect::<Result<_, _>>()?)
            }
            Value::Tagged(tagged) => Value::Tagged(Box::new(TaggedValue {
                tag: tagged.tag.clone(),
                value: ordered(&tagged.value)?,
            })),
            _ => value.clone(),
        })
    }

    // Map order is immaterial to key equality, including inside a sequence key.
    if matches!(key.untag_ref(), Value::Mapping(_) | Value::Sequence(_)) {
        oneyaml::to_string(&ordered(key)?)
    } else {
        oneyaml::to_string(key)
    }
}

/// JSON as a person writes it for a tool: comments and trailing commas are
/// allowed (tsconfig, VS Code settings), anything else malformed is not.
pub(crate) fn json_keys(text: &str) -> Result<KeyTally, String> {
    let mut tally = KeyTally::new();
    read_json(text, &mut tally, true)?;
    Ok(tally)
}

fn json_lines_keys(text: &str) -> Result<KeyTally, String> {
    let mut tally = KeyTally::new();
    for (i, line) in text.lines().enumerate() {
        read_json(line, &mut tally, false).map_err(|e| format!("line {}: {e}", i + 1))?;
    }
    Ok(tally)
}

fn read_json(text: &str, tally: &mut KeyTally, relaxed: bool) -> Result<(), String> {
    let mut walk = Json {
        s: text.as_bytes(),
        i: 0,
        tally,
        relaxed,
    };
    walk.skip();
    if !relaxed || walk.i < walk.s.len() {
        walk.value("", 0)?;
        walk.skip();
    }
    if walk.i < walk.s.len() {
        return Err(format!("unexpected text at byte {}", walk.i));
    }
    Ok(())
}

struct Json<'a> {
    s: &'a [u8],
    i: usize,
    tally: &'a mut KeyTally,
    relaxed: bool,
}

fn record_object(tally: &mut KeyTally, path: &str, counts: KeyTally) {
    for (key, count) in counts {
        let n = tally.entry(format!("{path}\u{1f}{key}")).or_default();
        *n = (*n).max(count);
    }
}

fn object_path(path: &str, key: &str) -> String {
    if key.contains(['.', '[', ']', '\u{1f}']) {
        format!("{path}[{}]", serde_json::to_string(key).unwrap())
    } else {
        format!("{path}.{key}")
    }
}

/// Deeper than this is not a file a person merges; refuse to guess.
const MAX_DEPTH: usize = 256;

impl Json<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    /// Whitespace and comments.
    fn skip(&mut self) {
        loop {
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                self.i += 1;
            }
            if !self.relaxed {
                return;
            }
            if self.s[self.i..].starts_with(b"//") {
                while self.peek().is_some_and(|b| !matches!(b, b'\n' | b'\r')) {
                    self.i += 1;
                }
            } else if self.s[self.i..].starts_with(b"/*") {
                match self.s[self.i + 2..].windows(2).position(|w| w == b"*/") {
                    Some(end) => self.i += 2 + end + 2,
                    None => return, // leave the unterminated comment for the caller to reject
                }
            } else {
                return;
            }
        }
    }

    fn comma(&mut self, close: u8) -> Result<(), String> {
        self.i += 1;
        if !self.relaxed {
            self.skip();
            if self.peek() == Some(close) {
                return Err(self.error("trailing comma"));
            }
        }
        Ok(())
    }

    fn error(&self, what: &str) -> String {
        match self.peek() {
            Some(b) => format!("{what} at byte {}, found `{}`", self.i, b as char),
            None => format!("{what}, found end of file"),
        }
    }

    fn value(&mut self, path: &str, depth: usize) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("nested too deeply".to_string());
        }
        match self.peek() {
            Some(b'{') => self.object(path, depth),
            Some(b'[') => self.array(path, depth),
            Some(b'"') => self.string().map(|_| ()),
            Some(b) if b == b'-' || b.is_ascii_alphanumeric() => {
                let start = self.i;
                while self
                    .peek()
                    .is_some_and(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
                {
                    self.i += 1;
                }
                serde_json::from_slice::<serde::de::IgnoredAny>(&self.s[start..self.i])
                    .map(|_| ())
                    .map_err(|e| format!("invalid value at byte {start}: {e}"))
            }
            _ => Err(self.error("expected a value")),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        let mut stream =
            serde_json::Deserializer::from_slice(&self.s[self.i..]).into_iter::<String>();
        let value = stream
            .next()
            .expect("positioned at opening quote")
            .map_err(|e| e.to_string())?;
        self.i += stream.byte_offset();
        Ok(value)
    }

    fn object(&mut self, path: &str, depth: usize) -> Result<(), String> {
        self.i += 1; // {
        let mut here = KeyTally::new();
        loop {
            self.skip();
            match self.peek() {
                Some(b'}') => {
                    self.i += 1;
                    break;
                }
                Some(b'"') => {}
                _ => return Err(self.error("expected a key or `}`")),
            }
            let key = self.string()?;
            self.skip();
            if self.peek() != Some(b':') {
                return Err(self.error("expected `:`"));
            }
            self.i += 1;
            self.skip();
            // A literal "a.b" or "rows[]" belongs to a different object than a nested path.
            let inner = if !matches!(self.peek(), Some(b'{' | b'[')) {
                String::new()
            } else {
                object_path(path, &key)
            };
            self.value(&inner, depth + 1)?;
            *here.entry(key).or_insert(0) += 1;
            self.skip();
            match self.peek() {
                Some(b',') => self.comma(b'}')?,
                Some(b'}') => {}
                _ => return Err(self.error("expected `,` or `}`")),
            }
        }
        record_object(self.tally, path, here);
        Ok(())
    }

    fn array(&mut self, path: &str, depth: usize) -> Result<(), String> {
        self.i += 1; // [
        let inner = format!("{path}[]");
        loop {
            self.skip();
            if self.peek() == Some(b']') {
                self.i += 1;
                return Ok(());
            }
            self.value(&inner, depth + 1)?;
            self.skip();
            match self.peek() {
                Some(b',') => self.comma(b']')?,
                Some(b']') => {}
                _ => return Err(self.error("expected `,` or `]`")),
            }
        }
    }
}

/// `"a.b\u{1f}k"` as the reader writes it: `a.b.k`.
pub fn display_key(entry: &str) -> String {
    match entry.split_once('\u{1f}') {
        Some(("", key)) => key.to_string(),
        Some((path, key)) => format!("{}.{key}", path.trim_start_matches('.')),
        None => entry.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tally(path: &str, text: &str) -> KeyTally {
        keys(path, text).expect("a data file").expect("parses")
    }

    #[test]
    fn a_json_key_stated_twice_in_one_object_is_counted_there() {
        let t = tally("p.json", "{\"a\": {\"k\": 1, \"k\": 2}, \"b\": {\"k\": 3}}");
        assert_eq!(t[".a\u{1f}k"], 2);
        assert_eq!(t[".b\u{1f}k"], 1);
        assert_eq!(t["\u{1f}a"], 1);
        assert_eq!(display_key(".a\u{1f}k"), "a.k");
    }

    #[test]
    fn json_array_elements_share_one_path() {
        let t = tally("p.json", "[{\"id\": 1}, {\"id\": 2, \"id\": 3}]");
        assert_eq!(t["[]\u{1f}id"], 2, "the most one element states it");
    }

    #[test]
    fn json_for_tools_may_carry_comments_and_trailing_commas() {
        let t = tally(
            "settings.JSONC",
            "// settings\n{\n  /* strict */ \"strict\": true,\n  \"paths\": [\"a\",],\n}\n",
        );
        assert_eq!(t["\u{1f}strict"], 1);
    }

    #[test]
    fn json5_counts_decoded_member_names_in_their_object() {
        let data = r#"{/* config */ rows: [{a: +.5}, {'a': Infinity, "\u0061": 0xff}], 'rows[]': {a: NaN}, ok: true, end: null, \u0062: -1, b: 2,}"#;
        let keys = tally("config.JsOn5", data);
        assert_eq!(keys[".rows[]\u{1f}a"], 2);
        assert_eq!(keys["\u{1f}b"], 2);
        assert_eq!(keys["[\"rows[]\"]\u{1f}a"], 1);
        assert!(super::keys("config.jsonc", data).unwrap().is_err());
        assert!(super::keys("json5", data).is_none());
        for bad in ["{a:}", "{} {}", "/*", ""] {
            assert!(super::keys("config.json5", bad).unwrap().is_err(), "{bad}");
        }
        let deep = format!(
            "{}0{}",
            "[".repeat(MAX_DEPTH + 1),
            "]".repeat(MAX_DEPTH + 1)
        );
        assert!(super::keys("config.json5", &deep).unwrap().is_err());
    }

    #[test]
    fn json_lines_counts_keys_per_record_and_requires_one_value_per_line() {
        for path in ["events.JSONL", "events.ndjson"] {
            let data = "{\"id\":1}\r\n{\"id\":2, \"nested\":{\"k\":1,\"k\":2}}\nnull\n";
            let keys = tally(path, data);
            assert_eq!(keys["\u{1f}id"], 1);
            assert_eq!(keys[".nested\u{1f}k"], 2);
            for line in [
                "",
                "\u{feff}{}",
                "{\"k\":1,}",
                "[1,]",
                "// note",
                "{",
                "true false",
            ] {
                let error = super::keys(path, &format!("0\n{line}\n"))
                    .unwrap()
                    .unwrap_err();
                assert!(error.starts_with("line 2:"), "{line:?}: {error}");
            }
        }
    }

    #[test]
    fn malformed_data_does_not_parse() {
        for (path, text) in [
            ("p.json", "{\"a\": 1 \"b\": 2}"),
            ("p.json", "{\"a\": }"),
            ("p.json", "{\"a\": 1"),
            ("p.json", "[01]"),
            ("p.json", "[1e+]"),
            ("p.json", "-no"),
            ("p.json", "{} /* unfinished"),
            ("p.json", "[1,\u{b}2]"),
            (
                "Cargo.toml",
                "[dependencies]\nserde = \"1\"\nserde = \"2\"\n",
            ),
            ("c.yaml", "a: 1\na: 2\n"),
            ("c.yaml", "a: [1, 2\n"),
            (
                "c.yaml",
                "---\nok: true\n---\nkey: \"value\"# missing space\n",
            ),
            ("c.yaml", "flow: [a,\nb,\nc]"),
            ("c.yaml", "outer: {kind: Pod, \"\\u006bind\": Service}"),
        ] {
            assert!(
                keys(path, text).expect("data").is_err(),
                "{path}: {text:?} must not parse"
            );
        }
    }

    #[test]
    fn wellformed_data_parses() {
        for (path, text) in [
            ("p.json", "{\"a\": [1, -2.5e3, true, null, \"x\\\"y\"]}"),
            ("p.json", ""),
            ("p.json", "[1e99999, // comment\r 2,]"),
            (
                "Cargo.toml",
                "[dependencies]\nserde = { version = \"1\" }\n",
            ),
            ("c.yml", "---\na: 1\n---\na: 2\n"),
            ("c.yaml", "ref: !Ref Thing\n"),
            ("c.yml", "---\n{\"multi\n  line\": a}\n---\n{multi line: b}"),
        ] {
            assert!(
                keys(path, text).expect("data").is_ok(),
                "{path}: {text:?} must parse"
            );
        }
        assert!(keys("m.py", "x = 1").is_none());
    }
}
