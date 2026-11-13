//! 時間の表現。
//!
//! 全ての時刻を「flick」(1/705,600,000 秒)の整数で持つ。
//! 24/25/30/48/50/60/120fps、30000/1001・60000/1001fps のフレーム、
//! 44.1k/48k/96kHz の標本が、どれも整数ちょうどの flick になるため、
//! 浮動小数点の誤差でクリップの位置がずれることがない。

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, Neg, Sub};

pub const FLICKS_PER_SECOND: i64 = 705_600_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Time(pub i64);

impl Time {
    pub const ZERO: Time = Time(0);

    pub fn from_secs_f64(s: f64) -> Time {
        Time((s * FLICKS_PER_SECOND as f64).round() as i64)
    }
    pub fn secs_f64(self) -> f64 {
        self.0 as f64 / FLICKS_PER_SECOND as f64
    }
    /// 100 ナノ秒単位(Media Foundation の時刻)との変換。
    pub fn from_hns(hns: i64) -> Time {
        Time((hns as i128 * FLICKS_PER_SECOND as i128 / 10_000_000) as i64)
    }
    pub fn hns(self) -> i64 {
        (self.0 as i128 * 10_000_000 / FLICKS_PER_SECOND as i128) as i64
    }
    pub fn max(self, o: Time) -> Time {
        Time(self.0.max(o.0))
    }
    pub fn min(self, o: Time) -> Time {
        Time(self.0.min(o.0))
    }
}

impl Add for Time {
    type Output = Time;
    fn add(self, o: Time) -> Time {
        Time(self.0 + o.0)
    }
}
impl Sub for Time {
    type Output = Time;
    fn sub(self, o: Time) -> Time {
        Time(self.0 - o.0)
    }
}
impl Neg for Time {
    type Output = Time;
    fn neg(self) -> Time {
        Time(-self.0)
    }
}

impl fmt::Display for Time {
    /// `HH:MM:SS.mmm`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let ms = (self.0.abs() as i128 * 1000 / FLICKS_PER_SECOND as i128) as i64;
        write!(f, "{sign}{:02}:{:02}:{:02}.{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
    }
}

/// `"00:01:02.500"`、`"62.5"`(秒)、`"62.5s"` を読む。
pub fn parse_time(s: &str) -> Option<Time> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let secs = if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() > 3 {
            return None;
        }
        let mut total = 0.0;
        for p in &parts {
            total = total * 60.0 + p.parse::<f64>().ok()?;
        }
        total
    } else {
        s.trim_end_matches('s').parse::<f64>().ok()?
    };
    if !secs.is_finite() {
        return None;
    }
    let t = Time::from_secs_f64(secs);
    Some(if neg { -t } else { t })
}

/// フレームレート(分数)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rate {
    pub num: u32,
    pub den: u32,
}

impl Rate {
    pub const fn new(num: u32, den: u32) -> Rate {
        Rate { num, den }
    }
    pub fn fps(self) -> f64 {
        self.num as f64 / self.den as f64
    }
    /// 1 フレームの長さ。
    pub fn frame_duration(self) -> Time {
        Time(FLICKS_PER_SECOND * self.den as i64 / self.num as i64)
    }
    /// フレーム番号 → 時刻(フレームの始まり)。
    pub fn frame_to_time(self, frame: i64) -> Time {
        Time(((frame as i128 * self.den as i128 * FLICKS_PER_SECOND as i128) / self.num as i128) as i64)
    }
    /// 時刻 → その時刻を含むフレームの番号。
    pub fn time_to_frame(self, t: Time) -> i64 {
        let n = t.0 as i128 * self.num as i128;
        let d = self.den as i128 * FLICKS_PER_SECOND as i128;
        n.div_euclid(d) as i64
    }
    /// 時刻をフレームの境界へ丸める(最も近い境界)。
    pub fn snap(self, t: Time) -> Time {
        let half = Time(self.frame_duration().0 / 2);
        self.frame_to_time(self.time_to_frame(t + half))
    }
}

/// 標本番号 ↔ 時刻(音声)。
pub fn sample_to_time(sample: i64, rate: u32) -> Time {
    Time(((sample as i128 * FLICKS_PER_SECOND as i128) / rate as i128) as i64)
}
pub fn time_to_sample(t: Time, rate: u32) -> i64 {
    ((t.0 as i128 * rate as i128).div_euclid(FLICKS_PER_SECOND as i128)) as i64
}
