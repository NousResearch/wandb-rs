use std::collections::HashMap;

use impl_from_tuple::impl_from_tuple;
use serde::{ser::SerializeMap, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq)]
pub enum DataValue {
    Bool(bool),
    Int(u64),
    SignedInt(i64),
    Float(f64),
    String(String),
    List(Vec<DataValue>),
    Dict(HashMap<String, DataValue>),
}

impl From<bool> for DataValue {
    fn from(v: bool) -> Self {
        DataValue::Bool(v)
    }
}

impl From<usize> for DataValue {
    fn from(v: usize) -> Self {
        DataValue::Int(v as u64)
    }
}
impl From<u64> for DataValue {
    fn from(v: u64) -> Self {
        DataValue::Int(v)
    }
}
impl From<u32> for DataValue {
    fn from(v: u32) -> Self {
        DataValue::Int(v.into())
    }
}
impl From<u16> for DataValue {
    fn from(v: u16) -> Self {
        DataValue::Int(v.into())
    }
}
impl From<u8> for DataValue {
    fn from(v: u8) -> Self {
        DataValue::Int(v.into())
    }
}

impl From<isize> for DataValue {
    fn from(v: isize) -> Self {
        DataValue::SignedInt(v as i64)
    }
}
impl From<i64> for DataValue {
    fn from(v: i64) -> Self {
        DataValue::SignedInt(v)
    }
}
impl From<i32> for DataValue {
    fn from(v: i32) -> Self {
        DataValue::SignedInt(v.into())
    }
}
impl From<i16> for DataValue {
    fn from(v: i16) -> Self {
        DataValue::SignedInt(v.into())
    }
}
impl From<i8> for DataValue {
    fn from(v: i8) -> Self {
        DataValue::SignedInt(v.into())
    }
}

impl From<f64> for DataValue {
    fn from(v: f64) -> Self {
        DataValue::Float(v)
    }
}
impl From<f32> for DataValue {
    fn from(v: f32) -> Self {
        DataValue::Float(v.into())
    }
}

impl From<String> for DataValue {
    fn from(v: String) -> Self {
        DataValue::String(v)
    }
}
impl From<&str> for DataValue {
    fn from(v: &str) -> Self {
        DataValue::String(v.to_string())
    }
}

impl<T: Into<DataValue>> From<Vec<T>> for DataValue {
    fn from(v: Vec<T>) -> Self {
        DataValue::List(v.into_iter().map(|x| x.into()).collect())
    }
}

impl<T: Into<DataValue>, S: Into<String>> From<HashMap<S, T>> for DataValue {
    fn from(v: HashMap<S, T>) -> Self {
        DataValue::Dict(v.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }
}

impl From<HashMap<String, DataValue>> for LogData {
    fn from(data: HashMap<String, DataValue>) -> Self {
        LogData { data }
    }
}

impl_from_tuple! { A }
impl_from_tuple! { A B }
impl_from_tuple! { A B C }
impl_from_tuple! { A B C D }
impl_from_tuple! { A B C D E }
impl_from_tuple! { A B C D E F }
impl_from_tuple! { A B C D E F G }
impl_from_tuple! { A B C D E F G H }
impl_from_tuple! { A B C D E F G H I }
impl_from_tuple! { A B C D E F G H I J }
impl_from_tuple! { A B C D E F G H I J K }
impl_from_tuple! { A B C D E F G H I J K L }
impl_from_tuple! { A B C D E F G H I J K L M }
impl_from_tuple! { A B C D E F G H I J K L M N }
impl_from_tuple! { A B C D E F G H I J K L M N O }
impl_from_tuple! { A B C D E F G H I J K L M N O P }
// lmnop should be enough :)

#[derive(Debug, Clone, PartialEq)]
pub struct LogData {
    data: HashMap<String, DataValue>,
}

impl Serialize for LogData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.data.len()))?;
        for (k, v) in &self.data {
            map.serialize_entry(k, &v)?;
        }
        map.end()
    }
}

impl Serialize for DataValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            DataValue::Bool(b) => serializer.serialize_bool(*b),
            DataValue::Int(i) => serializer.serialize_u64(*i),
            DataValue::SignedInt(i) => serializer.serialize_i64(*i),
            DataValue::Float(f) => serializer.serialize_f64(*f),
            DataValue::String(s) => serializer.serialize_str(s),
            DataValue::List(l) => l.serialize(serializer),
            DataValue::Dict(d) => {
                let mut map = serializer.serialize_map(Some(d.len()))?;
                for (k, v) in d {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn test_from_tuple() {
        let data: LogData = (
            ("bool", true),
            ("int", 1),
            ("float", 2.0),
            ("string", "three"),
            ("vec_int", vec![4, 5]),
            ("vec_string", vec!["six", "seven"]),
            (
                "hashmap_int",
                [("eight", 8), ("nine", 9)]
                    .into_iter()
                    .collect::<HashMap<_, _>>(),
            ),
        )
            .into();
        assert_eq!(
            data,
            LogData {
                data: HashMap::from([
                    ("bool".to_string(), DataValue::Bool(true)),
                    ("int".to_string(), DataValue::Int(1)),
                    ("float".to_string(), DataValue::Float(2.0)),
                    ("string".to_string(), DataValue::String("three".to_string())),
                    (
                        "vec_int".to_string(),
                        DataValue::List(vec![DataValue::Int(4), DataValue::Int(5)])
                    ),
                    (
                        "vec_string".to_string(),
                        DataValue::List(vec![
                            DataValue::String("six".to_string()),
                            DataValue::String("seven".to_string())
                        ])
                    ),
                    (
                        "hashmap_int".to_string(),
                        DataValue::Dict(HashMap::from([
                            ("eight".to_string(), DataValue::Int(8)),
                            ("nine".to_string(), DataValue::Int(9))
                        ]))
                    ),
                ])
            }
        )
    }
}
