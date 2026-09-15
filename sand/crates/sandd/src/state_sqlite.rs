use std::ffi::{CString, CStr};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::{Arc, Mutex};
use std::path::{Path, PathBuf};

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
    fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char;
}

#[derive(Debug)]
pub struct SqliteState {
    db: Arc<Mutex<*mut sqlite3>>,
    path: PathBuf,
}

unsafe impl Send for SqliteState {}
unsafe impl Sync for SqliteState {}

impl SqliteState {
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

        let state = Self {
            db: Arc::new(Mutex::new(db_ptr)),
            path,
        };

        // Enable WAL and create tables
        state.exec("PRAGMA journal_mode=WAL;")?;
        state.exec("PRAGMA synchronous=NORMAL;")?;
        state.exec("PRAGMA foreign_keys=ON;")?;

        state.exec(
            r#"
            CREATE TABLE IF NOT EXISTS runtime (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                state TEXT NOT NULL,
                workspace TEXT NOT NULL,
                cgroup_path TEXT,
                created_at INTEGER NOT NULL,
                started_at INTEGER,
                capabilities TEXT,
                procs INTEGER DEFAULT 0,
                ptys INTEGER DEFAULT 0,
                machine_id TEXT
            );
            CREATE TABLE IF NOT EXISTS process (
                id TEXT PRIMARY KEY,
                runtime_id TEXT NOT NULL,
                pid INTEGER NOT NULL,
                command TEXT,
                cwd TEXT,
                state TEXT,
                exit_code INTEGER,
                signal INTEGER,
                started_at INTEGER,
                exited_at INTEGER
            );
            CREATE TABLE IF NOT EXISTS pty (
                id TEXT PRIMARY KEY,
                runtime_id TEXT NOT NULL,
                pid INTEGER,
                cols INTEGER,
                rows INTEGER,
                cwd TEXT,
                shell TEXT,
                state TEXT
            );
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
            "#,
        )?;

        Ok(state)
    }

    fn exec(&self, sql: &str) -> Result<(), String> {
        let db_ptr = *self.db.lock().unwrap();
        let c_sql = CString::new(sql).map_err(|e| e.to_string())?;
        let mut errmsg: *mut c_char = std::ptr::null_mut();
        let rc = unsafe {
            sqlite3_exec(
                db_ptr,
                c_sql.as_ptr(),
                None,
                std::ptr::null_mut(),
                &mut errmsg as *mut *mut c_char,
            )
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

    fn exec_with_callback<F>(&self, sql: &str, mut callback: F) -> Result<(), String>
    where
        F: FnMut(Vec<(String, String)>) -> bool,
    {
        // Box the callback to pass via void*
        struct CallbackData<F> {
            cb: F,
        }

        extern "C" fn trampoline<F>(arg: *mut c_void, argc: c_int, argv: *mut *mut c_char, col_names: *mut *mut c_char) -> c_int
        where
            F: FnMut(Vec<(String, String)>) -> bool,
        {
            unsafe {
                let data = &mut *(arg as *mut CallbackData<F>);
                let mut row = Vec::new();
                for i in 0..argc {
                    let col_name_ptr = *col_names.offset(i as isize);
                    let val_ptr = *argv.offset(i as isize);
                    let col_name = if !col_name_ptr.is_null() {
                        CStr::from_ptr(col_name_ptr).to_string_lossy().to_string()
                    } else {
                        format!("col{}", i)
                    };
                    let val = if !val_ptr.is_null() {
                        CStr::from_ptr(val_ptr).to_string_lossy().to_string()
                    } else {
                        "".to_string()
                    };
                    row.push((col_name, val));
                }
                if (data.cb)(row) {
                    0
                } else {
                    1
                }
            }
        }

        let mut data = CallbackData { cb: callback };
        let db_ptr = *self.db.lock().unwrap();
        let c_sql = CString::new(sql).map_err(|e| e.to_string())?;
        let mut errmsg: *mut c_char = std::ptr::null_mut();
        let rc = unsafe {
            sqlite3_exec(
                db_ptr,
                c_sql.as_ptr(),
                Some(trampoline::<F>),
                &mut data as *mut _ as *mut c_void,
                &mut errmsg as *mut *mut c_char,
            )
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

    /// Add a column to an existing table when it is missing (SQLite has no
    /// `ADD COLUMN IF NOT EXISTS`, so the error is simply ignored).
    pub fn ensure_column(&self, table: &str, column: &str, decl: &str) -> Result<(), String> {
        self.exec(&format!(
            "ALTER TABLE {} ADD COLUMN {} {};",
            table, column, decl
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_runtime(&self, id: &str, kind: &str, state: &str, workspace: &str, cgroup_path: Option<&str>, created_at: u64, started_at: Option<u64>, capabilities: &str, procs: usize, ptys: usize, machine_id: &str) -> Result<(), String> {
        let sql = format!(
            "INSERT OR REPLACE INTO runtime (id, kind, state, workspace, cgroup_path, created_at, started_at, capabilities, procs, ptys, machine_id) VALUES ('{}', '{}', '{}', '{}', {}, {}, {}, '{}', {}, {}, '{}');",
            escape_sql(id),
            escape_sql(kind),
            escape_sql(state),
            escape_sql(workspace),
            cgroup_path.map(|p| format!("'{}'", escape_sql(p))).unwrap_or_else(|| "NULL".to_string()),
            created_at,
            started_at.map(|v| v.to_string()).unwrap_or_else(|| "NULL".to_string()),
            escape_sql(capabilities),
            procs,
            ptys,
            escape_sql(machine_id)
        );
        self.exec(&sql)
    }

    pub fn load_runtimes(&self) -> Result<Vec<RuntimeRow>, String> {
        let mut rows = Vec::new();
        self.exec_with_callback("SELECT id, kind, state, workspace, cgroup_path, created_at, started_at, capabilities, procs, ptys, machine_id FROM runtime;", |row| {
            let mut map = std::collections::HashMap::new();
            for (k, v) in row {
                map.insert(k, v);
            }
            let r = RuntimeRow {
                id: map.get("id").cloned().unwrap_or_default(),
                kind: map.get("kind").cloned().unwrap_or_default(),
                state: map.get("state").cloned().unwrap_or_default(),
                workspace: map.get("workspace").cloned().unwrap_or_default(),
                cgroup_path: map.get("cgroup_path").cloned().filter(|s| !s.is_empty()),
                created_at: map.get("created_at").and_then(|s| s.parse().ok()).unwrap_or(0),
                started_at: map.get("started_at").and_then(|s| s.parse().ok()),
                capabilities: map.get("capabilities").cloned().unwrap_or_default(),
                procs: map.get("procs").and_then(|s| s.parse().ok()).unwrap_or(0),
                ptys: map.get("ptys").and_then(|s| s.parse().ok()).unwrap_or(0),
                machine_id: map.get("machine_id").cloned().filter(|s| !s.is_empty()),
            };
            rows.push(r);
            true
        })?;
        Ok(rows)
    }

    pub fn remove_runtime(&self, id: &str) -> Result<(), String> {
        self.exec(&format!("DELETE FROM runtime WHERE id='{}';", escape_sql(id)))
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

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SqliteState {
    fn drop(&mut self) {
        let db_ptr = *self.db.lock().unwrap();
        if !db_ptr.is_null() {
            unsafe { sqlite3_close(db_ptr); }
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeRow {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub workspace: String,
    pub cgroup_path: Option<String>,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub capabilities: String,
    pub procs: usize,
    pub ptys: usize,
    /// Machine this runtime lives on (host fingerprint id), when known.
    pub machine_id: Option<String>,
}

fn escape_sql(s: &str) -> String {
    s.replace('\'', "''")
}
