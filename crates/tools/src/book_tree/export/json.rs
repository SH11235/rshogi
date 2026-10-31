use std::fmt::Write;

pub(super) enum Json {
    String(String),
    Number(String),
    Null,
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}
impl Json {
    pub fn string(s: impl Into<String>) -> Self {
        Self::String(s.into())
    }
    pub fn integer(n: impl ToString) -> Self {
        Self::Number(n.to_string())
    }
    pub fn object(fields: impl IntoIterator<Item = (impl Into<String>, Json)>) -> Self {
        Self::Object(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }
    pub fn render(&self) -> String {
        let mut text = String::new();
        self.write(&mut text, 0);
        text.push('\n');
        text
    }
    fn write(&self, out: &mut String, level: usize) {
        match self {
            Self::String(s) => out.push_str(&serde_json::to_string(s).expect("文字列")),
            Self::Number(s) => out.push_str(s),
            Self::Null => out.push_str("null"),
            Self::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('\n');
                    write!(out, "{:width$}", "", width = (level + 1) * 2).unwrap();
                    item.write(out, level + 1);
                }
                if !items.is_empty() {
                    out.push('\n');
                    write!(out, "{:width$}", "", width = level * 2).unwrap();
                }
                out.push(']');
            }
            Self::Object(items) => {
                out.push('{');
                for (i, (key, item)) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('\n');
                    write!(out, "{:width$}", "", width = (level + 1) * 2).unwrap();
                    out.push_str(&serde_json::to_string(key).expect("文字列"));
                    out.push_str(": ");
                    item.write(out, level + 1);
                }
                if !items.is_empty() {
                    out.push('\n');
                    write!(out, "{:width$}", "", width = level * 2).unwrap();
                }
                out.push('}');
            }
        }
    }
}
