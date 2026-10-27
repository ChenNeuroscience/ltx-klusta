//! Choose a stable desktop worker budget without changing per-recording maths.
use serde::Deserialize;
use std::str::FromStr;
use std::sync::OnceLock;

pub const MAX_JOBS: usize = 16;
const MAX_AUTO_JOBS: usize = 8;

fn budget(cpus: usize) -> usize {
    let cpus = cpus.max(1);
    cpus.saturating_sub(cpus.div_ceil(4))
        .clamp(1, MAX_AUTO_JOBS)
}

pub fn default_jobs() -> usize {
    static JOBS: OnceLock<usize> = OnceLock::new();
    *JOBS.get_or_init(|| budget(std::thread::available_parallelism().map_or(1, |n| n.get())))
}

#[derive(Clone, Copy, Debug)]
pub struct JobCount(pub usize);

impl FromStr for JobCount {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.eq_ignore_ascii_case("auto") {
            return Ok(Self(default_jobs()));
        }
        match text.parse::<usize>() {
            Ok(n) if (1..=MAX_JOBS).contains(&n) => Ok(Self(n)),
            _ => Err(format!(
                "jobs must be 'auto' or an integer between 1 and {MAX_JOBS}"
            )),
        }
    }
}

pub fn deserialize_jobs<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<usize, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Setting {
        Count(usize),
        Text(String),
    }
    let value = match Setting::deserialize(deserializer)? {
        Setting::Count(n) => n.to_string(),
        Setting::Text(text) => text,
    };
    value
        .parse::<JobCount>()
        .map(|n| n.0)
        .map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_budget_keeps_headroom_and_caps_large_machines() {
        for (cpus, expected) in [
            (0, 1),
            (1, 1),
            (2, 1),
            (3, 2),
            (4, 3),
            (8, 6),
            (12, 8),
            (64, 8),
        ] {
            assert_eq!(budget(cpus), expected);
        }
    }

    #[test]
    fn configuration_accepts_auto_and_fixed_limits() {
        use crate::pipeline::Settings;
        for json in ["{}", r#"{"jobs":"auto"}"#] {
            let settings: Settings = serde_json::from_str(json).unwrap();
            assert_eq!(settings.jobs, default_jobs());
        }
        let fixed: Settings = serde_json::from_str(r#"{"jobs":4}"#).unwrap();
        assert_eq!(fixed.jobs, 4);
        assert_eq!(serde_json::to_value(fixed).unwrap()["jobs"], 4);
        for json in [r#"{"jobs":0}"#, r#"{"jobs":17}"#, r#"{"jobs":"many"}"#] {
            assert!(serde_json::from_str::<Settings>(json).is_err());
        }
        assert_eq!("auto".parse::<JobCount>().unwrap().0, default_jobs());
        assert_eq!("16".parse::<JobCount>().unwrap().0, 16);
        assert!("0".parse::<JobCount>().is_err());
    }
}
