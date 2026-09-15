use sand_bridge as bridge;

use std::path::PathBuf;
use sand_client::SandClient;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_help();
        return;
    }

    let client = SandClient::new(None);

    match args[1].as_str() {
        "status" => {
            match client.status() {
                Ok(resp) => println!("{}", resp),
                Err(e) => eprintln!("error: {}", e),
            }
        }
        "runtime" => {
            if args.len() < 3 {
                eprintln!("usage: sand runtime <list|create|inspect|destroy|exec|ps>");
                return;
            }
            match args[2].as_str() {
                "list" => {
                    match client.list_runtimes() {
                        Ok(resp) => println!("{}", resp),
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
                "create" => {
                    let kind = if args.len() > 3 { args[3].as_str() } else { "assistant" };
                    let ws = if args.len() > 4 { Some(args[4].as_str()) } else { None };
                    match client.create_runtime(kind, ws) {
                        Ok(resp) => println!("{}", resp),
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
                "inspect" => {
                    if args.len() < 4 {
                        eprintln!("usage: sand runtime inspect <id>");
                        return;
                    }
                    match client.get_runtime(&args[3]) {
                        Ok(resp) => println!("{}", resp),
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
                "destroy" => {
                    if args.len() < 4 {
                        eprintln!("usage: sand runtime destroy <id>");
                        return;
                    }
                    match client.destroy_runtime(&args[3]) {
                        Ok(resp) => println!("{}", resp),
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
                "exec" => {
                    if args.len() < 5 {
                        eprintln!("usage: sand runtime exec <id> <command...>");
                        return;
                    }
                    let id = &args[3];
                    let cmd: Vec<String> = args[4..].to_vec();
                    match client.exec(id, cmd) {
                        Ok(resp) => {
                            println!("{}", resp);
                            // try to decode stdout_b64
                            if let Some(stdout_b64) = extract_field(&resp, "stdout_b64") {
                                let data = base64_decode(&stdout_b64);
                                println!("--- stdout ---\n{}", String::from_utf8_lossy(&data));
                            }
                            if let Some(stderr_b64) = extract_field(&resp, "stderr_b64") {
                                let data = base64_decode(&stderr_b64);
                                if !data.is_empty() {
                                    println!("--- stderr ---\n{}", String::from_utf8_lossy(&data));
                                }
                            }
                        }
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
                "ps" => {
                    if args.len() < 4 {
                        eprintln!("usage: sand runtime ps <id>");
                        return;
                    }
                    // for now, get_runtime includes procs
                    match client.get_runtime(&args[3]) {
                        Ok(resp) => println!("{}", resp),
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
                "kill" => {
                    if args.len() < 4 {
                        eprintln!("usage: sand runtime kill <id>");
                        return;
                    }
                    match client.destroy_runtime(&args[3]) {
                        Ok(resp) => println!("{}", resp),
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
                _ => {
                    eprintln!("unknown runtime subcommand {}", args[2]);
                }
            }
        }
        "pty" => {
            if args.len() < 3 {
                eprintln!("usage: sand pty <list|open|close|resize|write>");
                return;
            }
            match args[2].as_str() {
                "list" => {
                    if args.len() < 4 {
                        eprintln!("usage: sand pty list <runtime_id>");
                        return;
                    }
                    match client.list_ptys(&args[3]) {
                        Ok(resp) => println!("{}", resp),
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
                "open" => {
                    if args.len() < 5 {
                        eprintln!("usage: sand pty open <runtime_id> <pty_id>");
                        return;
                    }
                    let cols = 80;
                    let rows = 24;
                    match client.open_pty(&args[3], &args[4], cols, rows) {
                        Ok(resp) => println!("{}", resp),
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
                "close" => {
                    if args.len() < 5 {
                        eprintln!("usage: sand pty close <runtime_id> <pty_id>");
                        return;
                    }
                    match client.close_pty(&args[3], &args[4]) {
                        Ok(resp) => println!("{}", resp),
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
                "resize" => {
                    if args.len() < 6 {
                        eprintln!("usage: sand pty resize <runtime_id> <pty_id> <cols> <rows>");
                        return;
                    }
                    let cols: u16 = args[5].parse().unwrap_or(80);
                    let rows: u16 = if args.len() > 6 { args[6].parse().unwrap_or(24) } else { 24 };
                    match client.resize_pty(&args[3], &args[4], cols, rows) {
                        Ok(resp) => println!("{}", resp),
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
                _ => {
                    eprintln!("unknown pty subcommand {}", args[2]);
                }
            }
        }
        // SSH stdio ⇄ sandd UDS tunnel. Runs over the SSH connection that the
        // host-agent establishes; never opens a network port.
        "bridge" => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            let options = match bridge::options_from_args(args) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("error: {}", e);
                    eprintln!("usage: sand bridge [--socket <sandd.sock>] [-v]");
                    std::process::exit(2);
                }
            };
            if let Err(e) = bridge::run(options) {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
        }
        "logs" => {
            eprintln!("logs not yet implemented, use runtime inspect");
        }
        _ => {
            print_help();
        }
    }
}

fn print_help() {
    println!("sand - Runtime Kernel CLI");
    println!("");
    println!("Usage:");
    println!("  sand status");
    println!("  sand runtime list");
    println!("  sand runtime create [assistant|task|workbench|eval] [workspace]");
    println!("  sand runtime inspect <id>");
    println!("  sand runtime destroy <id>");
    println!("  sand runtime exec <id> <command...>");
    println!("  sand runtime ps <id>");
    println!("  sand runtime kill <id>");
    println!("  sand pty list <runtime_id>");
    println!("  sand pty open <runtime_id> <pty_id>");
    println!("  sand pty close <runtime_id> <pty_id>");
    println!("  sand pty resize <runtime_id> <pty_id> <cols> <rows>");
    println!("  sand bridge [--socket <sandd.sock>] [-v]   (SSH stdio ⇄ sandd UDS)");
}

fn extract_field(s: &str, field: &str) -> Option<String> {
    let pat = format!("\"{}\":\"", field);
    if let Some(start) = s.find(&pat) {
        let rest = &s[start + pat.len()..];
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_string());
        }
    }
    None
}

fn base64_decode(s: &str) -> Vec<u8> {
    let mut table = [255u8; 256];
    for (i, &c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate() {
        table[c as usize] = i as u8;
    }
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i]==b'\n' || bytes[i]==b'\r' || bytes[i]==b' ') { i+=1; }
        if i+3 >= bytes.len() { break; }
        let mut vals = [0u8; 4];
        let mut padding = 0;
        let mut valid = true;
        for j in 0..4 {
            if i+j >= bytes.len() { valid=false; break; }
            let b = bytes[i+j];
            if b == b'=' {
                padding+=1;
                vals[j]=0;
            } else {
                let v = table[b as usize];
                if v==255 { valid=false; break; }
                vals[j]=v;
            }
        }
        if !valid { i+=1; continue; }
        let n = ((vals[0] as u32)<<18) | ((vals[1] as u32)<<12) | ((vals[2] as u32)<<6) | (vals[3] as u32);
        out.push(((n>>16)&0xFF) as u8);
        if padding<2 { out.push(((n>>8)&0xFF) as u8); }
        if padding<1 { out.push((n&0xFF) as u8); }
        i+=4;
        if padding>0 { break; }
    }
    out
}
