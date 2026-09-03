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

    #[cfg(all(any(target_os = "linux", target_os = "macos"), feature = "fuse"))]
    {
        println!("\nFUSE:");
        #[cfg(target_os = "linux")]
        {
            let helper = std::path::Path::new("/usr/bin/fusermount3").exists()
                || std::path::Path::new("/bin/fusermount3").exists();
            println!("  fusermount3 present: {helper}");
            if !helper {
                println!("  hint: install fuse3 (Debian/Ubuntu: apt install fuse3)");
            }
        }
        #[cfg(target_os = "macos")]
        println!("  requires macFUSE: https://macfuse.io (5.2+ avoids the kernel extension)");
    }

    #[cfg(all(windows, feature = "projfs"))]
    {
        let ok = anymount::probe::projfs();
        println!("\nProjFS available: {ok}");
        if !ok {
            println!(
                "  hint: Enable-WindowsOptionalFeature -Online \
                 -FeatureName Client-ProjFS -NoRestart  (admin, no reboot)"
            );
        }
    }

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
