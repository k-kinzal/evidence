//! Tables: CSV, TSV, JSON, JSON Lines, and TOML subjects as rows and columns, and SQL over
//! them (an in-memory SQLite database per query).

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Map, Value};

/// The source format of a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TableKind {
    /// Comma-separated values with a header row.
    Csv,
    /// Tab-separated values with a header row.
    Tsv,
    /// A JSON document.
    Json,
    /// One JSON value per line.
    Jsonl,
    /// A TOML document.
    Toml,
}

impl TableKind {
    /// From a file name.
    pub fn from_path(path: &str) -> Option<Self> {
        let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
        Some(match ext.as_str() {
            "csv" => Self::Csv,
            "tsv" | "tab" => Self::Tsv,
            "json" => Self::Json,
            "jsonl" | "ndjson" => Self::Jsonl,
            "toml" => Self::Toml,
            _ => return None,
        })
    }

    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
            Self::Toml => "toml",
        }
    }
}

/// Rows and columns. Cells are JSON values (nested values stay nested).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Table {
    /// Column names.
    pub columns: Vec<String>,
    /// Rows, each as long as `columns`.
    pub rows: Vec<Vec<Value>>,
}

impl Table {
    /// The table as `{columns, rows, total, truncated}` with at most `max_rows` rows.
    pub fn to_json(&self, max_rows: usize) -> Value {
        let rows: Vec<&Vec<Value>> = self.rows.iter().take(max_rows).collect();
        serde_json::json!({
            "columns": self.columns,
            "rows": rows,
            "total": self.rows.len(),
            "truncated": self.rows.len() > max_rows,
        })
    }

    /// Rows as objects (for charts).
    pub fn records(&self, max_rows: usize) -> Value {
        Value::Array(
            self.rows
                .iter()
                .take(max_rows)
                .map(|row| {
                    Value::Object(
                        self.columns
                            .iter()
                            .cloned()
                            .zip(row.iter().cloned())
                            .collect::<Map<String, Value>>(),
                    )
                })
                .collect(),
        )
    }

    /// The first `max_rows` rows as CSV text (for agents reading a table).
    pub fn to_csv(&self, max_rows: usize) -> String {
        let mut w = csv::Writer::from_writer(Vec::new());
        let _ = w.write_record(&self.columns);
        for row in self.rows.iter().take(max_rows) {
            let _ = w.write_record(row.iter().map(cell_text));
        }
        String::from_utf8(w.into_inner().unwrap_or_default()).unwrap_or_default()
    }

    fn from_records(values: Vec<Value>) -> Self {
        let mut columns: Vec<String> = Vec::new();
        let objects = values.iter().all(Value::is_object);
        if !objects {
            return Self {
                columns: vec!["value".into()],
                rows: values.into_iter().map(|v| vec![v]).collect(),
            };
        }
        for v in &values {
            if let Value::Object(o) = v {
                for k in o.keys() {
                    if !columns.contains(k) {
                        columns.push(k.clone());
                    }
                }
            }
        }
        let rows = values
            .into_iter()
            .map(|v| match v {
                Value::Object(mut o) => columns
                    .iter()
                    .map(|c| o.remove(c).unwrap_or(Value::Null))
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        Self { columns, rows }
    }

    fn from_value(value: Value) -> Self {
        match value {
            Value::Array(items) => Self::from_records(items),
            Value::Object(o) => {
                // The largest array of objects inside, or the object's entries.
                let best = o
                    .iter()
                    .filter_map(|(k, v)| match v {
                        Value::Array(a) if !a.is_empty() && a.iter().all(Value::is_object) => {
                            Some((k.clone(), a.len()))
                        }
                        _ => None,
                    })
                    .max_by_key(|(_, n)| *n);
                let mut o = o;
                if let Some((key, _)) = best
                    && let Some(Value::Array(items)) = o.remove(&key)
                {
                    return Self::from_records(items);
                }
                Self {
                    columns: vec!["key".into(), "value".into()],
                    rows: o
                        .into_iter()
                        .map(|(k, v)| vec![Value::String(k), v])
                        .collect(),
                }
            }
            other => Self {
                columns: vec!["value".into()],
                rows: vec![vec![other]],
            },
        }
    }
}

/// A cell as text: strings as they are, everything else as JSON, null as empty.
pub fn cell_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Numbers in delimited text become numbers; empty cells become null.
fn typed(cell: &str) -> Value {
    let t = cell.trim();
    if t.is_empty() {
        return Value::Null;
    }
    if let Ok(i) = t.parse::<i64>() {
        return Value::from(i);
    }
    if let Ok(f) = t.parse::<f64>()
        && f.is_finite()
        && !t.starts_with('+')
    {
        return Value::from(f);
    }
    Value::String(cell.to_owned())
}

/// Parses a table.
pub fn parse(kind: TableKind, text: &str) -> Result<Table> {
    match kind {
        TableKind::Csv | TableKind::Tsv => {
            let mut reader = csv::ReaderBuilder::new()
                .delimiter(if kind == TableKind::Tsv { b'\t' } else { b',' })
                .flexible(true)
                .from_reader(text.as_bytes());
            let mut columns: Vec<String> = reader
                .headers()
                .context("reading the header row")?
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let h = h.trim().trim_start_matches('\u{feff}');
                    if h.is_empty() {
                        format!("column {}", i + 1)
                    } else {
                        h.to_owned()
                    }
                })
                .collect();
            let mut rows = Vec::new();
            for record in reader.records() {
                let record = record.context("reading a row")?;
                let row: Vec<Value> = record.iter().map(typed).collect();
                while row.len() > columns.len() {
                    columns.push(format!("column {}", columns.len() + 1));
                }
                rows.push(row);
            }
            for row in &mut rows {
                row.resize(columns.len(), Value::Null);
            }
            dedupe(&mut columns);
            Ok(Table { columns, rows })
        }
        TableKind::Json => {
            let v: Value = serde_json::from_str(text).context("parsing JSON")?;
            Ok(Table::from_value(v))
        }
        TableKind::Jsonl => {
            let mut values = Vec::new();
            for (n, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                values.push(serde_json::from_str(line).with_context(|| format!("line {}", n + 1))?);
            }
            Ok(Table::from_records(values))
        }
        TableKind::Toml => {
            let v: toml::Value = toml::from_str(text).context("parsing TOML")?;
            let v = serde_json::to_value(v)?;
            Ok(Table::from_value(v))
        }
    }
}

fn dedupe(columns: &mut [String]) {
    let mut seen = std::collections::HashMap::<String, usize>::new();
    for c in columns.iter_mut() {
        let n = seen.entry(c.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            *c = format!("{c} ({n})");
        }
    }
}

/// SQL table name for a page id: `.` and `-` become `_`.
pub fn sql_name(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Runs one read-only SQL statement over `tables` (name, table). At most `max_rows` rows are
/// returned; `truncated` in the result says whether there were more.
pub fn query(tables: &[(String, &Table)], sql: &str, max_rows: usize) -> Result<(Table, bool)> {
    use rusqlite::limits::Limit;
    use rusqlite::types::Value as Sql;

    let conn = rusqlite::Connection::open_in_memory()?;
    // No ATTACH (it could create files) and bounded work.
    conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, 50_000_000)?;
    for (name, table) in tables {
        let cols: Vec<String> = table.columns.iter().map(|c| quote_ident(c)).collect();
        conn.execute_batch(&format!(
            "CREATE TABLE {} ({});",
            quote_ident(name),
            if cols.is_empty() {
                "\"value\"".to_owned()
            } else {
                cols.join(", ")
            }
        ))?;
        if table.columns.is_empty() {
            continue;
        }
        let tx = conn.unchecked_transaction()?;
        {
            let placeholders = vec!["?"; table.columns.len()].join(", ");
            let mut insert = tx.prepare(&format!(
                "INSERT INTO {} VALUES ({placeholders})",
                quote_ident(name)
            ))?;
            for row in &table.rows {
                let values: Vec<Sql> = row
                    .iter()
                    .map(|v| match v {
                        Value::Null => Sql::Null,
                        Value::Bool(b) => Sql::Integer(i64::from(*b)),
                        Value::Number(n) => n
                            .as_i64()
                            .map(Sql::Integer)
                            .or_else(|| n.as_f64().map(Sql::Real))
                            .unwrap_or(Sql::Null),
                        Value::String(s) => Sql::Text(s.clone()),
                        other => Sql::Text(other.to_string()),
                    })
                    .collect();
                insert.execute(rusqlite::params_from_iter(values))?;
            }
        }
        tx.commit()?;
    }
    conn.execute_batch("PRAGMA query_only = 1;")?;
    let sql = sql.trim().trim_end_matches(';');
    let mut stmt = conn.prepare(sql)?;
    if !stmt.readonly() {
        bail!("only read-only statements (SELECT, WITH …) are allowed");
    }
    let columns: Vec<String> = stmt.column_names().map(str::to_owned).collect();
    let n = columns.len();
    let mut rows = Vec::new();
    let mut truncated = false;
    let mut result = stmt.query([])?;
    while let Some(row) = result.next()? {
        if rows.len() >= max_rows {
            truncated = true;
            break;
        }
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let v: Sql = row.get(i)?;
            out.push(match v {
                Sql::Null => Value::Null,
                Sql::Integer(i) => Value::from(i),
                Sql::Real(f) => Value::from(f),
                Sql::Text(s) => Value::String(s),
                Sql::Blob(b) => Value::String(format!("<{} bytes>", b.len())),
            });
        }
        rows.push(out);
    }
    Ok((Table { columns, rows }, truncated))
}

fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_delimited_with_types() {
        let t = parse(
            TableKind::Csv,
            "name,score,,name\na,1,x,b\nb,2.5,,c,extra\n",
        )
        .unwrap();
        assert_eq!(
            t.columns,
            vec!["name", "score", "column 3", "name (2)", "column 5"]
        );
        assert_eq!(t.rows[0][1], Value::from(1));
        assert_eq!(t.rows[1][1], Value::from(2.5));
        assert_eq!(t.rows[1][2], Value::Null);
        assert_eq!(t.rows[0][4], Value::Null);
        let tsv = parse(TableKind::Tsv, "a\tb\n1\t2\n").unwrap();
        assert_eq!(tsv.rows, vec![vec![Value::from(1), Value::from(2)]]);
    }

    #[test]
    fn parses_json_shapes() {
        let t = parse(TableKind::Json, r#"[{"a":1},{"b":{"c":2}}]"#).unwrap();
        assert_eq!(t.columns, vec!["a", "b"]);
        assert_eq!(t.rows[1][1], serde_json::json!({"c": 2}));
        let t = parse(TableKind::Json, r#"{"meta":1,"items":[{"x":1},{"x":2}]}"#).unwrap();
        assert_eq!((t.columns.clone(), t.rows.len()), (vec!["x".to_owned()], 2));
        let t = parse(TableKind::Json, r#"{"a":1,"b":[1,2]}"#).unwrap();
        assert_eq!(t.columns, vec!["key", "value"]);
        let t = parse(TableKind::Jsonl, "{\"a\":1}\n\n{\"a\":2}\n").unwrap();
        assert_eq!(t.rows.len(), 2);
        let t = parse(
            TableKind::Toml,
            "[[pkg]]\nname = \"x\"\n[[pkg]]\nname = \"y\"\n",
        )
        .unwrap();
        assert_eq!(t.rows.len(), 2);
    }

    #[test]
    fn queries_read_only() {
        let t = parse(TableKind::Csv, "name,score\na,1\nb,5\nc,3\n").unwrap();
        let tables = vec![("t".to_owned(), &t), (sql_name("my.scores-2"), &t)];
        let (r, truncated) = query(
            &tables,
            "SELECT name FROM t WHERE score > 2 ORDER BY score DESC",
            10,
        )
        .unwrap();
        assert_eq!(r.rows, vec![vec![Value::from("b")], vec![Value::from("c")]]);
        assert!(!truncated);
        let (r, truncated) = query(&tables, "SELECT * FROM my_scores_2", 2).unwrap();
        assert_eq!((r.rows.len(), truncated), (2, true));
        assert!(query(&tables, "DELETE FROM t", 10).is_err());
        assert!(query(&tables, "ATTACH DATABASE 'x.db' AS x", 10).is_err());
    }
}
