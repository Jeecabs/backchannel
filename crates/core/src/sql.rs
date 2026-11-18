//! The small slice of SQLite that both rusqlite and Durable Object storage can provide.

use serde_json::{Map, Value};

use crate::types::Result;

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Null,
    Int(i64),
    Text(String),
}

impl From<i64> for Val {
    fn from(v: i64) -> Self {
        Val::Int(v)
    }
}
impl From<u32> for Val {
    fn from(v: u32) -> Self {
        Val::Int(v as i64)
    }
}
impl From<bool> for Val {
    fn from(v: bool) -> Self {
        Val::Int(v as i64)
    }
}
impl From<&str> for Val {
    fn from(v: &str) -> Self {
        Val::Text(v.to_string())
    }
}
impl From<String> for Val {
    fn from(v: String) -> Self {
        Val::Text(v)
    }
}
impl From<&String> for Val {
    fn from(v: &String) -> Self {
        Val::Text(v.clone())
    }
}
impl<T: Into<Val>> From<Option<T>> for Val {
    fn from(v: Option<T>) -> Self {
        v.map(Into::into).unwrap_or(Val::Null)
    }
}

pub type Row = Map<String, Value>;

pub trait Sql {
    /// Runs several `;`-separated statements with no parameters.
    fn batch(&self, sql: &str) -> Result<()>;
    fn query(&self, sql: &str, params: &[Val]) -> Result<Vec<Row>>;
    fn run(&self, sql: &str, params: &[Val]) -> Result<()>;
    /// Opens a transaction. Returns false when one is already open (or the
    /// backend is implicitly atomic, like a Durable Object between awaits).
    fn begin(&self) -> Result<bool>;
    fn commit(&self) -> Result<()>;
    fn rollback(&self);

    fn one(&self, sql: &str, params: &[Val]) -> Result<Option<Row>> {
        Ok(self.query(sql, params)?.into_iter().next())
    }
}

/// Runs `f` atomically. Nested calls join the outer transaction.
pub fn tx<S: Sql + ?Sized, T>(s: &S, f: impl FnOnce() -> Result<T>) -> Result<T> {
    if !s.begin()? {
        return f();
    }
    match f() {
        Ok(v) => {
            s.commit()?;
            Ok(v)
        }
        Err(e) => {
            s.rollback();
            Err(e)
        }
    }
}

#[macro_export]
macro_rules! p {
    ($($x:expr),* $(,)?) => { &[$($crate::sql::Val::from($x)),*] };
}

// Row accessors. Durable Object rows arrive as JS numbers, so integers may be floats.

pub fn int(r: &Row, k: &str) -> i64 {
    opt_int(r, k).unwrap_or(0)
}

pub fn opt_int(r: &Row, k: &str) -> Option<i64> {
    match r.get(k)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::Bool(b) => Some(*b as i64),
        _ => None,
    }
}

pub fn text(r: &Row, k: &str) -> String {
    opt_text(r, k).unwrap_or_default()
}

pub fn opt_text(r: &Row, k: &str) -> Option<String> {
    match r.get(k)? {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}
