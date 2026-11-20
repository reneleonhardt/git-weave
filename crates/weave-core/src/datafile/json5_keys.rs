//! Visit members before a value map can discard duplicate keys.
use super::{object_path, record_object, KeyTally, MAX_DEPTH};
use serde::de::{self, Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use std::fmt;

pub(super) fn parse(text: &str) -> Result<KeyTally, String> {
    json5::from_str::<Document>(text)
        .map(|doc| doc.0)
        .map_err(|e| e.to_string())
}

struct Document(KeyTally);

impl<'de> Deserialize<'de> for Document {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let mut tally = KeyTally::new();
        Node {
            path: "",
            depth: 0,
            tally: &mut tally,
        }
        .deserialize(de)?;
        Ok(Self(tally))
    }
}

struct Node<'a> {
    path: &'a str,
    depth: usize,
    tally: &'a mut KeyTally,
}

impl<'de> DeserializeSeed<'de> for Node<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<(), D::Error> {
        if self.depth > MAX_DEPTH {
            return Err(de::Error::custom("nested too deeply"));
        }
        de.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Node<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON5 value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut counts = KeyTally::new();
        while let Some(key) = map.next_key::<String>()? {
            map.next_value_seed(Node {
                path: &object_path(self.path, &key),
                depth: self.depth + 1,
                tally: self.tally,
            })?;
            *counts.entry(key).or_default() += 1;
        }
        record_object(self.tally, self.path, counts);
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let path = format!("{}[]", self.path);
        while seq
            .next_element_seed(Node {
                path: &path,
                depth: self.depth + 1,
                tally: self.tally,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
}
