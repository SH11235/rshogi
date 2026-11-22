//! `.params` の読み取り。build script (`RSHOGI_EMBED_SPSA_PARAMS` の焼き込み) と
//! エンジン (`SPSAParamsFile`) で同じ規則を使うため、両方から読み込む。

/// `.params` (name,type,value,min,max,c_end,r_end [// comment] [[[NOT USED]]]) から名前と値を取り出す。
pub fn parse_spsa_params(content: &str) -> Vec<(String, i32)> {
    let mut entries = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let val_part = trimmed.split("//").next().unwrap_or(trimmed);
        let val_part = val_part.replace("[[NOT USED]]", "");
        let cols: Vec<&str> = val_part.split(',').map(str::trim).collect();
        if cols.len() < 3 {
            continue;
        }
        let parsed = if cols[1].eq_ignore_ascii_case("int") {
            match cols[2].parse::<f64>() {
                Ok(v) => v.round() as i32,
                Err(_) => continue,
            }
        } else {
            match cols[2].parse::<i32>() {
                Ok(v) => v,
                Err(_) => continue,
            }
        };
        entries.push((cols[0].to_string(), parsed));
    }
    entries
}
