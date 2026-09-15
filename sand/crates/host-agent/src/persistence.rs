use std::ffi::{CString, CStr};
use std::os::raw::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[repr(C)]
struct sqlite3 { _private: [u8; 0] }

#[link(name = "sqlite3")]
extern "C" {
    fn sqlite3_open(filename: *const c_char, ppDb: *mut *mut sqlite3) -> c_int;
    fn sqlite3_close(db: *mut sqlite3) -> c_int;
    fn sqlite3_exec(
        db: *mut sqlite3,
        sql: *const c_char,
        callback: Option<extern "C" fn(*mut c_void, c_int, *mut *mut c_char, *mut *mut c_char) -> c_int>,
        arg: *mut c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_free(ptr: *mut c_void);
}

pub struct SqlitePersistence {
    db: Arc<Mutex<*mut sqlite3>>,
    path: PathBuf,
}

unsafe impl Send for SqlitePersistence {}
unsafe impl Sync for SqlitePersistence {}

impl SqlitePersistence {
    pub fn new(path: PathBuf) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut db_ptr: *mut sqlite3 = std::ptr::null_mut();
        let c_path = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let rc = unsafe { sqlite3_open(c_path.as_ptr(), &mut db_ptr) };
        if rc != 0 {
            return Err(format!("sqlite open failed rc={}", rc));
        }
        let state = Self { db: Arc::new(Mutex::new(db_ptr)), path };
        state.exec("PRAGMA journal_mode=WAL;")?;
        state.exec("PRAGMA synchronous=NORMAL;")?;
        state.exec(
            r#"
            CREATE TABLE IF NOT EXISTS session (
                id TEXT PRIMARY KEY,
                runtime_id TEXT NOT NULL,
                model TEXT,
                status TEXT,
                goal TEXT,
                cwd TEXT,
                created_at INTEGER,
                updated_at INTEGER,
                messages TEXT
            );
            CREATE TABLE IF NOT EXISTS task (
                id TEXT PRIMARY KEY,
                goal TEXT NOT NULL,
                session_id TEXT NOT NULL,
                runtime_id TEXT NOT NULL,
                status TEXT NOT NULL,
                artifacts TEXT,
                result TEXT,
                created_at INTEGER,
                updated_at INTEGER
            );
            CREATE TABLE IF NOT EXISTS event (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                runtime_id TEXT,
                session_id TEXT,
                kind TEXT NOT NULL,
                payload TEXT,
                created_at INTEGER NOT NULL
            );
            "#
        )?;
        Ok(state)
    }

    fn exec(&self, sql: &str) -> Result<(), String> {
        let db_ptr = *self.db.lock().unwrap();
        let c_sql = CString::new(sql).map_err(|e| e.to_string())?;
        let mut errmsg: *mut c_char = std::ptr::null_mut();
        let rc = unsafe {
            sqlite3_exec(db_ptr, c_sql.as_ptr(), None, std::ptr::null_mut(), &mut errmsg as *mut *mut c_char)
        };
        if rc != 0 {
            let msg = if !errmsg.is_null() {
                let c_str = unsafe { CStr::from_ptr(errmsg) };
                let s = c_str.to_string_lossy().to_string();
                unsafe { sqlite3_free(errmsg as *mut c_void) };
                s
            } else {
                format!("sqlite exec failed rc={}", rc)
            };
            return Err(msg);
        }
        Ok(())
    }

    pub fn save_session(&self, id: &str, runtime_id: &str, model: &str, status: &str, goal: Option<&str>, cwd: &str, created_at: u64, updated_at: u64, messages: &str) -> Result<(), String> {
        let sql = format!(
            "INSERT OR REPLACE INTO session (id, runtime_id, model, status, goal, cwd, created_at, updated_at, messages) VALUES ('{}', '{}', '{}', '{}', {}, '{}', {}, {}, '{}');",
            escape_sql(id),
            escape_sql(runtime_id),
            escape_sql(model),
            escape_sql(status),
            goal.map(|g| format!("'{}'", escape_sql(g))).unwrap_or_else(|| "NULL".to_string()),
            escape_sql(cwd),
            created_at,
            updated_at,
            escape_sql(messages)
        );
        self.exec(&sql)
    }

    pub fn save_task(&self, id: &str, goal: &str, session_id: &str, runtime_id: &str, status: &str, artifacts: &str, result: Option<&str>, created_at: u64, updated_at: u64) -> Result<(), String> {
        let sql = format!(
            "INSERT OR REPLACE INTO task (id, goal, session_id, runtime_id, status, artifacts, result, created_at, updated_at) VALUES ('{}', '{}', '{}', '{}', '{}', '{}', {}, {}, {});",
            escape_sql(id),
            escape_sql(goal),
            escape_sql(session_id),
            escape_sql(runtime_id),
            escape_sql(status),
            escape_sql(artifacts),
            result.map(|r| format!("'{}'", escape_sql(r))).unwrap_or_else(|| "NULL".to_string()),
            created_at,
            updated_at
        );
        self.exec(&sql)
    }

    pub fn push_event(&self, runtime_id: Option<&str>, session_id: Option<&str>, kind: &str, payload: &str) -> Result<(), String> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let sql = format!(
            "INSERT INTO event (runtime_id, session_id, kind, payload, created_at) VALUES ({}, {}, '{}', '{}', {});",
            runtime_id.map(|id| format!("'{}'", escape_sql(id))).unwrap_or_else(|| "NULL".to_string()),
            session_id.map(|id| format!("'{}'", escape_sql(id))).unwrap_or_else(|| "NULL".to_string()),
            escape_sql(kind),
            escape_sql(payload),
            now
        );
        self.exec(&sql)
    }
}

impl Drop for SqlitePersistence {
    fn drop(&mut self) {
        let db_ptr = *self.db.lock().unwrap();
        if !db_ptr.is_null() {
            unsafe { sqlite3_close(db_ptr); }
        }
    }
}

fn escape_sql(s: &str) -> String {
    s.replace('\'', "''")
}
