use anyhow::{Context, Result, ensure};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{ToPrimitive, Zero};
use serde_json::value::RawValue;

pub(super) fn repr(value: f64) -> String {
    let shortest = format!("{value:e}");
    let (mantissa, _) = shortest.split_once('e').unwrap();
    let digits = mantissa.bytes().filter(u8::is_ascii_digit).count();
    // 最短桁数が同じ候補の中でも、十進の中点は偶数側を選ぶ。
    let scientific = format!("{:.*e}", digits - 1, value);
    let (mantissa, exponent) = scientific.split_once('e').unwrap();
    let exponent: i32 = exponent.parse().unwrap();
    if !(-4..16).contains(&exponent) {
        format!("{mantissa}e{exponent:+03}")
    } else {
        let text = format!("{:.*}", (digits as i32 - 1 - exponent).max(0) as usize, value);
        if text.contains('.') {
            text
        } else {
            format!("{text}.0")
        }
    }
}

pub(super) fn decimal(text: &str) -> Result<BigRational> {
    let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((text, "0"));
    let exponent: i32 = exponent.parse()?;
    let decimals = mantissa.split_once('.').map_or(0, |(_, f)| f.len() as i32);
    let numerator: BigInt = mantissa.replace('.', "").parse()?;
    let power = exponent.checked_sub(decimals).context("numeric exponent overflow")?;
    // 有限な f64 の十進表現と JSON 整数だけを受け取る。
    if power >= 0 {
        Ok(BigRational::from_integer(numerator * BigInt::from(10).pow(power as u32)))
    } else {
        Ok(BigRational::new(numerator, BigInt::from(10).pow(power.unsigned_abs())))
    }
}

pub(super) fn number(value: &RawValue) -> Result<BigRational> {
    let text = value.get();
    ensure!(
        text.starts_with(|c: char| c == '-' || c.is_ascii_digit()),
        "expected finite number"
    );
    if !text.contains(['.', 'e', 'E']) {
        let integer: BigInt = text.parse()?;
        ensure!(integer.to_f64().is_some_and(f64::is_finite), "expected finite number");
        return Ok(BigRational::from_integer(integer));
    }
    let float: f64 = text.parse()?;
    ensure!(float.is_finite(), "expected finite number");
    decimal(&repr(float))
}
pub(super) fn mean(values: &[BigRational]) -> Result<f64> {
    ensure!(!values.is_empty(), "empty mean");
    let sum: BigRational = values.iter().cloned().sum();
    let value = (sum / BigInt::from(values.len())).to_f64().context("value overflow")?;
    ensure!(value.is_finite(), "value overflow");
    // 二進浮動小数を十進小数 1 桁に丸める。先に 10 倍すると境界が変わる。
    Ok(format!("{value:.1}").parse()?)
}
pub(super) fn rounded_nodes(nodes: &BigRational) -> Result<String> {
    let value = nodes.to_f64().context("nodes overflow")?;
    ensure!(value.is_finite(), "nodes overflow");
    Ok(format!("{value:.1e}"))
}
pub(super) fn eval_number(value: f64) -> String {
    if value.fract().is_zero() {
        BigRational::from_float(value).unwrap().to_integer().to_string()
    } else {
        repr(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_numeric_lexemes() {
        for (input, expected) in [
            ("184467440737095516160", "184467440737095516160"),
            ("1.00000000000000011102230246251565404236316680908203126", "1.0000000000000002"),
            ("9007199254740993.0", "9007199254740992"),
            ("1e-400", "0"),
            ("-0", "0"),
            ("-0.0", "0"),
        ] {
            let raw: Box<RawValue> = serde_json::from_str(input).unwrap();
            assert_eq!(number(&raw).unwrap(), decimal(expected).unwrap(), "{input}");
        }
        for input in [r#""12""#, "true", "false", "null", "[]", "{}", "1e400"] {
            let raw: Box<RawValue> = serde_json::from_str(input).unwrap();
            assert!(number(&raw).is_err(), "{input}");
        }
    }

    #[test]
    fn decimal_rounding_and_rendering() {
        for (value, expected) in [
            (12.0, "12.0"),
            (f64::from_bits(4833185809221898721), "1331752170181752.2"),
            (-0.0, "-0.0"),
            (1e-5, "1e-05"),
            (1e-4, "0.0001"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
        ] {
            assert_eq!(repr(value), expected);
        }
        for (s, expected) in [
            ("2.25", 2.2),
            ("2.35", 2.4),
            ("2.55", 2.5),
            ("-2.55", -2.5),
            ("12", 12.0),
        ] {
            assert_eq!(mean(&[decimal(s).unwrap()]).unwrap(), expected);
        }
        assert_eq!(eval_number(-0.0), "0");
        assert_eq!(eval_number(1e20), "100000000000000000000");
        assert_eq!(rounded_nodes(&decimal("1250000000").unwrap()).unwrap(), "1.2e9");
        assert_eq!(rounded_nodes(&decimal("1350000000").unwrap()).unwrap(), "1.4e9");
        assert_eq!(
            number(&serde_json::from_str::<Box<RawValue>>("9007199254740993").unwrap()).unwrap(),
            decimal("9007199254740993").unwrap()
        );
        assert!(number(&serde_json::from_str::<Box<RawValue>>("true").unwrap()).is_err());
        assert!(number(&serde_json::from_str::<Box<RawValue>>("1e400").unwrap()).is_err());
    }
}
