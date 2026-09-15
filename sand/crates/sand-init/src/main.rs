use std::path::PathBuf;
use std::fs;

fn main() {
    println!("[sand-init] initializing cgroup and runtime dirs");

    // Try to create /sys/fs/cgroup/sand
    let cgroup_root = PathBuf::from("/sys/fs/cgroup/sand");
    match fs::create_dir_all(&cgroup_root) {
        Ok(_) => println!("[sand-init] created {}", cgroup_root.display()),
        Err(_) => {
            // try sudo
            let _ = std::process::Command::new("sudo")
                .args(&["mkdir", "-p", cgroup_root.to_str().unwrap()])
                .output();
            println!("[sand-init] attempted sudo mkdir for {}", cgroup_root.display());
        }
    }

    // Ensure /run/sand
    let run_sand = PathBuf::from("/run/sand");
    let _ = fs::create_dir_all(&run_sand);
    let _ = std::process::Command::new("sudo")
        .args(&["mkdir", "-p", "/run/sand"])
        .output();
    let _ = std::process::Command::new("sudo")
        .args(&["chmod", "777", "/run/sand"])
        .output();

    // Ensure /tmp/sandd
    let _ = fs::create_dir_all("/tmp/sandd");
    let _ = fs::create_dir_all("/tmp/sand-cgroups");

    println!("[sand-init] done");
}
