//! Durable Object SQLite behind backchannel_core's `Sql` trait.
//!
//! A Durable Object runs one event at a time and only interleaves at `await`
//! points. All core logic is synchronous, so each call is already atomic;
//! `begin` reports that no explicit transaction is needed.

use backchannel_core::sql::{Row, Sql, Val};
use backchannel_core::{Code, Error, Result};
use serde_json::{Map, Value};
use worker::{SqlStorage, SqlStorageValue};

pub struct DoSql(pub SqlStorage);

fn bind(params: &[Val]) -> Vec<SqlStorageValue> {
    params
        .iter()
        .map(|v| match v {
            Val::Null => SqlStorageValue::Null,
            Val::Int(i) => SqlStorageValue::Integer(*i),
            Val::Text(s) => SqlStorageValue::String(s.clone()),
        })
        .collect()
}

fn e(err: worker::Error) -> Error {
    Error::new(Code::Internal, format!("storage: {err}"))
}

impl Sql for DoSql {
    fn batch(&self, sql: &str) -> Result<()> {
        for stmt in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            self.0
                .exec(stmt, Vec::new())
                .map_err(e)?
                .to_array::<Value>()
                .map_err(e)?;
        }
        Ok(())
    }

    fn query(&self, sql: &str, params: &[Val]) -> Result<Vec<Row>> {
        let rows: Vec<Value> = self
            .0
            .exec(sql, bind(params))
            .map_err(e)?
            .to_array()
            .map_err(e)?;
        Ok(rows
            .into_iter()
            .map(|r| match r {
                Value::Object(m) => m,
                _ => Map::new(),
            })
            .collect())
    }

    fn run(&self, sql: &str, params: &[Val]) -> Result<()> {
        self.0
            .exec(sql, bind(params))
            .map_err(e)?
            .to_array::<Value>()
            .map_err(e)?;
        Ok(())
    }

    fn begin(&self) -> Result<bool> {
        Ok(false)
    }

    fn commit(&self) -> Result<()> {
        Ok(())
    }

    fn rollback(&self) {}
}
