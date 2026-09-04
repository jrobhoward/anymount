//! Phase 0 spike driver: report what this machine can actually mount.
//!
//! Run this first on each OS. It needs no mountpoint and no privileges, so it
//! isolates "are the build and runtime dependencies present?" from "does
//! mounting work?".
//!
//! ```sh
//! cargo run --example probe
//! ```

fn main() {
    println!("anymount backend probe");
    println!(
        "  target: {} / {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!(
        "  a backend is compiled in: {}",
        anymount::probe::any_backend_available()
    );

    #[cfg(all(target_os = "linux", feature = "fuse"))]
    {
        println!("\nFUSE:");
        let helper = std::path::Path::new("/usr/bin/fusermount3").exists()
            || std::path::Path::new("/bin/fusermount3").exists();
        println!("  fusermount3 present: {helper}");
        if !helper {
            println!("  hint: install fuse3 (Debian/Ubuntu: apt install fuse3)");
        }
    }

    // macOS has no backend in the tree yet — the decided mechanism is NFS,
    // not FUSE (docs/PLAN.md) — so nothing to probe there today.
    #[cfg(target_os = "macos")]
    println!("\nno backend compiled in for macOS yet (decided: NFS; see docs/PLAN.md)");

    #[cfg(all(windows, feature = "cfapi"))]
    {
        match anymount::probe::cfapi() {
            Some(info) => {
                println!("\nCloud Files (cfapi) available:");
                println!("  build {}.{}", info.build, info.revision);
                println!("  integration 0x{:x}", info.integration);
            }
            None => println!("\nCloud Files (cfapi): unavailable (needs Windows 10 1709+)"),
        }
    }
}
