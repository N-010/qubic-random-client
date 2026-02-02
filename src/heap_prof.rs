use crate::config::Config;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::runtime::Handle;

static HEAP_STATS_ENABLED: AtomicBool = AtomicBool::new(false);

#[cfg(all(feature = "jemalloc", not(target_env = "msvc")))]
mod imp {
    use std::time::Duration;

    use tokio::runtime::Handle;

    use crate::config::Config;

    pub(super) fn setup(handle: &Handle, config: &Config) {
        if config.heap_dump
            && let Err(err) = dump_once()
        {
            eprintln!("heap dump failed: {err}");
        }

        if config.heap_dump_interval_secs == 0 {
            return;
        }

        let interval = Duration::from_secs(config.heap_dump_interval_secs);
        handle.spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                if let Err(err) = dump_once() {
                    eprintln!("heap dump failed: {err}");
                }
            }
        });
    }

    pub(super) fn print_stats() {
        #[cfg(not(target_env = "msvc"))]
        {
            if let Err(err) = tikv_jemalloc_ctl::stats_print::stats_print(
                std::io::stdout(),
                tikv_jemalloc_ctl::stats_print::Options::default(),
            ) {
                eprintln!("jemalloc stats print failed: {err}");
            }
        }
    }

    pub(super) fn on_shutdown() {}

    fn dump_once() -> Result<(), String> {
        tikv_jemalloc_ctl::raw::write_str(b"prof.dump\0", b"\0")
            .map_err(|err| format!("jemalloc prof dump failed: {err}"))
    }
}

#[cfg(not(all(feature = "jemalloc", not(target_env = "msvc"))))]
mod imp {
    use tokio::runtime::Handle;

    use crate::config::Config;

    pub(super) fn setup(_handle: &Handle, config: &Config) {
        if config.heap_dump || config.heap_dump_interval_secs > 0 {
            eprintln!(
                "heap dump requested but jemalloc profiling is not available on this platform"
            );
            #[cfg(all(target_os = "windows", feature = "mimalloc"))]
            eprintln!("tip: set MIMALLOC_SHOW_STATS=1 to print allocator stats at exit");
        }
        #[cfg(not(all(target_os = "windows", feature = "mimalloc")))]
        if config.heap_stats {
            eprintln!("heap stats requested but allocator stats are not available");
        }
    }

    pub(super) fn print_stats() {
        #[cfg(all(target_os = "windows", feature = "mimalloc"))]
        unsafe {
            libmimalloc_sys::mi_stats_print(std::ptr::null_mut());
        }
    }

    pub(super) fn on_shutdown() {
        #[cfg(all(target_os = "windows", feature = "mimalloc"))]
        {
            if should_print_mimalloc_stats() {
                unsafe {
                    libmimalloc_sys::mi_stats_print(std::ptr::null_mut());
                }
            }
        }
    }

    #[cfg(all(target_os = "windows", feature = "mimalloc"))]
    fn should_print_mimalloc_stats() -> bool {
        match std::env::var("MIMALLOC_SHOW_STATS") {
            Ok(value) => {
                let value = value.trim();
                !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
            }
            Err(_) => false,
        }
    }
}

pub fn setup(handle: &Handle, config: &Config) {
    HEAP_STATS_ENABLED.store(config.heap_stats, Ordering::Relaxed);
    imp::setup(handle, config);
}

pub fn on_shutdown() {
    if HEAP_STATS_ENABLED.load(Ordering::Relaxed) {
        imp::print_stats();
    } else {
        imp::on_shutdown();
    }
}
