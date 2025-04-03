use serde::{Deserialize, Deserializer};

pub fn deserialize_null_into_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    let opt = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

pub fn is_default<T: Default + PartialEq>(t: &T) -> bool {
    t == &T::default()
}

pub fn deserialize_duration<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    let value: Result<i64, D::Error> = Deserialize::deserialize(deserializer);
    match value {
        Ok(v) => {
            if v < 0 {
                return Ok(0);
            }
            Ok(v)
        }
        Err(_) => Ok(0),
    }
}
