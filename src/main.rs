use std::net::{IpAddr, UdpSocket};
use std::time::Duration;
use sysinfo::{Disks, System};
use serde::Serialize;
use nix::sys::statvfs::statvfs;

// --- Data Structures ---

// Sent exactly once at startup
#[derive(Serialize)]
struct StaticOsInfo {
    vm_ip: IpAddr,
    os_name: String,
    os_version: String,
    os_codename: Option<String>,
}

#[derive(Serialize)]
struct MemoryInfo {
    total_kb: u64,
    used_kb: u64,
}

#[derive(Serialize)]
struct DiskInfo {
    mount_point: String,
    total_space_gb: u64,
    available_space_gb: u64,
}

#[derive(Serialize)]
struct InodeInfo {
    total: u64,
    free: u64,
}

// Sent repeatedly every 60 seconds
#[derive(Serialize)]
struct DynamicMetrics {
    vm_ip: IpAddr,
    ram: MemoryInfo,
    disks: Vec<DiskInfo>,
    inodes: Option<InodeInfo>,
}

// Finds the IP this VM uses to reach the server.
// UDP connect sends no packets; it only asks the OS which route and interface it would use.
fn local_ip_towards(server: &str) -> std::io::Result<IpAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.connect((server, 8080))?;
    Ok(sock.local_addr()?.ip())
}

fn main() {
    let server_ip = "127.0.0.1";
    // Adjust endpoints matching your central server's API layout (e.g. custom ports)
    let register_url = format!("http://{}:3000/api/register", server_ip);
    let metrics_url = format!("http://{}:3000/api/metrics", server_ip);

    // The VM's IP is its ID on the server, so it goes in every payload.
    // The IP is fixed, so we look it up once.
    let vm_ip = match local_ip_towards(server_ip) {
        Ok(ip) => ip,
        Err(e) => {
            eprintln!("Failed to detect this VM's IP address: {}", e);
            return;
        }
    };
    println!("This VM's IP: {}", vm_ip);

    // Initialize the blocking HTTP client
    let client = reqwest::blocking::Client::new();

    // ==========================================
    // PHASE 1: Send Structural OS Info Once
    // ==========================================
    let os = os_info::get();
    let static_payload = StaticOsInfo {
        vm_ip,
        os_name: format!("{}", os.os_type()),
        os_version: format!("{}", os.version()),
        os_codename: os.codename().map(|s| s.to_string()),
    };

    println!("Sending structural OS configuration metadata once...");
    match client.post(&register_url).json(&static_payload).send() {
        Ok(res) => println!("Registration successful: Target server responded with status {}", res.status()),
        Err(e) => eprintln!("Warning: Failed to deliver registration payload: {}", e),
    }

    // ==========================================
    // PHASE 2: Continual System Metrics Loop
    // ==========================================
    let mut sys = match System::new_all() {
        Ok(system) => system,
        Err(e) => {
            eprintln!("Failed to initialize system monitoring framework: {}", e);
            return;
        }
    };

    loop {
        // Refresh metrics context
        sys.refresh_all();

        // 1. Gather RAM metrics
        let ram = MemoryInfo {
            total_kb: sys.total_memory() / 1024,
            used_kb: sys.used_memory() / 1024,
        };

        // 2. Gather storage space metrics
        let mut disks = Vec::new();
        if let Ok(disk_list) = Disks::new_with_refreshed_list() {
            for disk in &disk_list {
                disks.push(DiskInfo {
                    mount_point: disk.mount_point().to_string_lossy().into_owned(),
                    total_space_gb: disk.total_space() / 1024 / 1024 / 1024,
                    available_space_gb: disk.available_space() / 1024 / 1024 / 1024,
                });
            }
        }

        // 3. Gather active Inode allocations (UNIX specific)
        #[cfg(unix)]
        let inodes = statvfs("/").ok().map(|stats| InodeInfo {
            total: stats.files(),
            free: stats.files_free(),
        });
        #[cfg(not(unix))]
        let inodes = None;

        // 4. Construct the metric payload
        let metrics_payload = DynamicMetrics { vm_ip, ram, disks, inodes };

        // 5. Transmit metrics payload synchronously
        match client.post(&metrics_url).json(&metrics_payload).send() {
            Ok(_) => println!("Metrics payload successfully transmitted."),
            Err(e) => eprintln!("Network Error: Unable to reach metrics ingestion line: {}", e),
        }

        // Freeze the worker thread physically for 60 seconds
        std::thread::sleep(Duration::from_secs(60));
    }
}
