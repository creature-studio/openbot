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

/// Row collector used by `sqlite3_exec`'s callback.
#[derive(Default)]
struct Rows {
    rows: Vec<Vec<Option<String>>>,
}

/// `extern "C"` callback: collect one row per call.
extern "C" fn collect_row(
    ctx: *mut c_void,
    argc: c_int,
    argv: *mut *mut c_char,
    _col_names: *mut *mut c_char,
) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    let rows = unsafe { &mut *(ctx as *mut Rows) };
    let mut row = Vec::with_capacity(argc as usize);
    for index in 0..argc {
        let value_ptr = unsafe { *argv.offset(index as isize) };
        if value_ptr.is_null() {
            row.push(None);
        } else {
            let value = unsafe { CStr::from_ptr(value_ptr) };
            row.push(Some(value.to_string_lossy().to_string()));
        }
    }
    rows.rows.push(row);
    0
}

#[derive(Debug, Clone)]
pub struct PersistedSession {
    pub id: String,
    pub runtime_id: String,
    pub machine_id: String,
    pub model: String,
    pub status: String,
    pub goal: Option<String>,
    pub cwd: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub messages: String,
}

#[derive(Debug, Clone)]
pub struct PersistedTask {
    pub id: String,
    pub goal: String,
    pub session_id: String,
    pub runtime_id: String,
    pub machine_id: String,
    pub status: String,
    pub artifacts: String,
    pub result: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
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
                machine_id TEXT NOT NULL DEFAULT 'machine-local',
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
                machine_id TEXT NOT NULL DEFAULT 'machine-local',
                status TEXT NOT NULL,
                artifacts TEXT,
                result TEXT,
                created_at INTEGER,
                updated_at INTEGER
            );
            CREATE TABLE IF NOT EXISTS machines (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                host TEXT NOT NULL,
                port INTEGER NOT NULL,
                user TEXT,
                ssh_config_host TEXT,
                display_name TEXT,
                created_at INTEGER
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
        // Existing installations predate machine pinning. SQLite has no
        // `ADD COLUMN IF NOT EXISTS`, so an already-migrated database simply
        // returns an ignorable "duplicate column" error for each statement.
        let _ = state.exec("ALTER TABLE session ADD COLUMN machine_id TEXT NOT NULL DEFAULT 'machine-local';");
        let _ = state.exec("ALTER TABLE task ADD COLUMN machine_id TEXT NOT NULL DEFAULT 'machine-local';");
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

    pub fn save_session(&self, id: &str, runtime_id: &str, machine_id: &str, model: &str, status: &str, goal: Option<&str>, cwd: &str, created_at: u64, updated_at: u64, messages: &str) -> Result<(), String> {
        let sql = format!(
            "INSERT OR REPLACE INTO session (id, runtime_id, machine_id, model, status, goal, cwd, created_at, updated_at, messages) VALUES ('{}', '{}', '{}', '{}', '{}', {}, '{}', {}, {}, '{}');",
            escape_sql(id),
            escape_sql(runtime_id),
            escape_sql(machine_id),
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

    pub fn save_task(&self, id: &str, goal: &str, session_id: &str, runtime_id: &str, machine_id: &str, status: &str, artifacts: &str, result: Option<&str>, created_at: u64, updated_at: u64) -> Result<(), String> {
        let sql = format!(
            "INSERT OR REPLACE INTO task (id, goal, session_id, runtime_id, machine_id, status, artifacts, result, created_at, updated_at) VALUES ('{}', '{}', '{}', '{}', '{}', '{}', '{}', {}, {}, {});",
            escape_sql(id),
            escape_sql(goal),
            escape_sql(session_id),
            escape_sql(runtime_id),
            escape_sql(machine_id),
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

    /// Run a query and collect every row (`sqlite3_exec` + callback).
    fn query(&self, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
        let db_ptr = *self.db.lock().unwrap();
        if db_ptr.is_null() {
            return Err("sqlite database is closed".to_string());
        }
        let c_sql = CString::new(sql).map_err(|e| e.to_string())?;
        let mut rows = Rows::default();
        let mut errmsg: *mut c_char = std::ptr::null_mut();
        let rc = unsafe {
            sqlite3_exec(
                db_ptr,
                c_sql.as_ptr(),
                Some(collect_row),
                &mut rows as *mut Rows as *mut c_void,
                &mut errmsg as *mut *mut c_char,
            )
        };
        if rc != 0 {
            let message = if !errmsg.is_null() {
                let text = unsafe { CStr::from_ptr(errmsg) }
                    .to_string_lossy()
                    .to_string();
                unsafe { sqlite3_free(errmsg as *mut c_void) };
                text
            } else {
                format!("sqlite query failed rc={rc}")
            };
            return Err(message);
        }
        Ok(rows.rows)
    }

    // -----------------------------------------------------------------------
    // Session / task recovery
    // -----------------------------------------------------------------------

    pub fn load_sessions(&self) -> Result<Vec<PersistedSession>, String> {
        let rows = self.query(
            "SELECT id, runtime_id, machine_id, model, status, goal, cwd, created_at, updated_at, messages FROM session ORDER BY created_at ASC;",
        )?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                if row.len() < 10 { return None; }
                Some(PersistedSession {
                    id: row[0].clone().unwrap_or_default(),
                    runtime_id: row[1].clone().unwrap_or_default(),
                    machine_id: row[2].clone().unwrap_or_else(|| "machine-local".to_string()),
                    model: row[3].clone().unwrap_or_default(),
                    status: row[4].clone().unwrap_or_else(|| "idle".to_string()),
                    goal: row[5].clone(),
                    cwd: row[6].clone().unwrap_or_else(|| "/workspace".to_string()),
                    created_at: row[7].as_deref().and_then(|v| v.parse().ok()).unwrap_or(0),
                    updated_at: row[8].as_deref().and_then(|v| v.parse().ok()).unwrap_or(0),
                    messages: row[9].clone().unwrap_or_else(|| "[]".to_string()),
                })
            })
            .collect())
    }

    pub fn load_tasks(&self) -> Result<Vec<PersistedTask>, String> {
        let rows = self.query(
            "SELECT id, goal, session_id, runtime_id, machine_id, status, artifacts, result, created_at, updated_at FROM task ORDER BY created_at ASC;",
        )?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                if row.len() < 10 { return None; }
                Some(PersistedTask {
                    id: row[0].clone().unwrap_or_default(),
                    goal: row[1].clone().unwrap_or_default(),
                    session_id: row[2].clone().unwrap_or_default(),
                    runtime_id: row[3].clone().unwrap_or_default(),
                    machine_id: row[4].clone().unwrap_or_else(|| "machine-local".to_string()),
                    status: row[5].clone().unwrap_or_else(|| "pending".to_string()),
                    artifacts: row[6].clone().unwrap_or_else(|| "[]".to_string()),
                    result: row[7].clone(),
                    created_at: row[8].as_deref().and_then(|v| v.parse().ok()).unwrap_or(0),
                    updated_at: row[9].as_deref().and_then(|v| v.parse().ok()).unwrap_or(0),
                })
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Machines
    // -----------------------------------------------------------------------

    /// Persist a machine record.
    ///
    /// Only connection *coordinates* are stored — id, name, host, port, user and
    /// the `~/.ssh/config` alias. There is no column for a password, a
    /// passphrase or a private key, and the schema is the enforcement
    /// (architecture §九).
    pub fn save_machine(&self, machine: &spark_model::Machine) -> Result<(), String> {
        let (host, port, user, alias) = match &machine.kind {
            spark_model::MachineKind::Ssh {
                host,
                port,
                user,
                ssh_config_host,
            } => (
                host.clone(),
                *port,
                user.clone(),
                ssh_config_host.clone(),
            ),
            // The local machine is implicit; storing it would only create a
            // record that can drift from the running host-agent.
            spark_model::MachineKind::Local => return Ok(()),
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let sql = format!(
            "INSERT OR REPLACE INTO machines (id, name, host, port, user, ssh_config_host, display_name, created_at) \
             VALUES ('{}', '{}', '{}', {}, {}, {}, {}, {});",
            escape_sql(machine.id.as_str()),
            escape_sql(&machine.name),
            escape_sql(&host),
            port,
            user.map(|u| format!("'{}'", escape_sql(&u)))
                .unwrap_or_else(|| "NULL".to_string()),
            alias
                .map(|a| format!("'{}'", escape_sql(&a)))
                .unwrap_or_else(|| "NULL".to_string()),
            machine
                .display_name
                .as_ref()
                .map(|d| format!("'{}'", escape_sql(d)))
                .unwrap_or_else(|| "NULL".to_string()),
            now
        );
        self.exec(&sql)
    }

    /// Every saved machine, ready to register with MachineManager.
    pub fn load_machines(&self) -> Result<Vec<spark_model::Machine>, String> {
        let rows = self.query(
            "SELECT id, name, host, port, user, ssh_config_host FROM machines ORDER BY created_at ASC;",
        )?;
        let mut machines = Vec::new();
        for row in rows {
            if row.len() < 6 {
                continue;
            }
            let id = row[0].clone().unwrap_or_default();
            let name = row[1].clone().unwrap_or_default();
            let host = row[2].clone().unwrap_or_default();
            let port = row[3]
                .clone()
                .and_then(|p| p.parse::<u16>().ok())
                .unwrap_or(22);
            let user = row[4].clone();
            let alias = row[5].clone();
            machines.push(spark_model::Machine::ssh(
                spark_model::MachineId::from_string(id),
                name,
                host,
                port,
                user.filter(|u| !u.is_empty()),
                alias.filter(|a| !a.is_empty()),
            ));
        }
        Ok(machines)
    }

    pub fn delete_machine(&self, machine_id: &str) -> Result<(), String> {
        self.exec(&format!(
            "DELETE FROM machines WHERE id = '{}';",
            escape_sql(machine_id)
        ))
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
