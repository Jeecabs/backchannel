//! rusqlite storage for native builds. One file can be shared by many processes (WAL).

use std::cell::Cell;
use std::path::Path;

use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::Connection;
use serde_json::{Map, Value};

use crate::sql::{Row, Sql, Val};
use crate::types::{Code, Error, Result};

pub struct Sqlite {
    conn: Connection,
    depth: Cell<u32>,
}

fn db_err(e: rusqlite::Error) -> Error {
    Error::new(Code::Internal, format!("sqlite: {e}"))
}

impl Sqlite {
    pub fn open(path: &Path) -> Result<Sqlite> {
        let conn = Connection::open(path).map_err(db_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(10)).map_err(db_err)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;").map_err(db_err)?;
        Ok(Sqlite { conn, depth: Cell::new(0) })
    }

    pub fn memory() -> Result<Sqlite> {
        Ok(Sqlite { conn: Connection::open_in_memory().map_err(db_err)?, depth: Cell::new(0) })
    }
}

fn to_sql(v: &Val) -> SqlValue {
    match v {
        Val::Null => SqlValue::Null,
        Val::Int(i) => SqlValue::Integer(*i),
        Val::Text(s) => SqlValue::Text(s.clone()),
    }
}

impl Sql for Sqlite {
    fn batch(&self, sql: &str) -> Result<()> {
        self.conn.execute_batch(sql).map_err(db_err)
    }

    fn query(&self, sql: &str, params: &[Val]) -> Result<Vec<Row>> {
        let mut stmt = self.conn.prepare_cached(sql).map_err(db_err)?;
        let names: Vec<String> = stmt.column_names().into_iter().map(str::to_string).collect();
        let vals: Vec<SqlValue> = params.iter().map(to_sql).collect();
        let mut rows = stmt.query(rusqlite::params_from_iter(vals)).map_err(db_err)?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().map_err(db_err)? {
            let mut m = Map::new();
            for (i, n) in names.iter().enumerate() {
                let v = match r.get_ref(i).map_err(db_err)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(x) => Value::from(x),
                    ValueRef::Real(x) => Value::from(x),
                    ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
                    ValueRef::Blob(_) => Value::Null,
                };
                m.insert(n.clone(), v);
            }
            out.push(m);
        }
        Ok(out)
    }

    fn run(&self, sql: &str, params: &[Val]) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(sql).map_err(db_err)?;
        let vals: Vec<SqlValue> = params.iter().map(to_sql).collect();
        stmt.execute(rusqlite::params_from_iter(vals)).map_err(db_err)?;
        Ok(())
    }

    fn begin(&self) -> Result<bool> {
        let d = self.depth.get();
        self.depth.set(d + 1);
        if d > 0 {
            return Ok(false);
        }
        if let Err(e) = self.conn.execute_batch("BEGIN IMMEDIATE") {
            self.depth.set(0);
            return Err(db_err(e));
        }
        Ok(true)
    }

    fn commit(&self) -> Result<()> {
        self.depth.set(0);
        self.conn.execute_batch("COMMIT").map_err(db_err)
    }

    fn rollback(&self) {
        self.depth.set(0);
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}
