//! Обёртки над системными API Windows.
//!
//! Единственный крейт Bamboo, где разрешён `unsafe`. Во всех остальных стоит
//! `#![forbid(unsafe_code)]` — это делает аудит выполнимым: чтобы проверить
//! проект на корректность работы с памятью, достаточно прочитать один крейт.
//!
//! Наружу отдаются только безопасные типы из `bamboo-core`.

#![cfg(windows)]

pub mod apps;
pub mod boot;
pub mod budget;
pub mod clock;
pub mod cmdline;
pub mod control;
pub mod cpu;
pub mod digest;
pub mod etw;
pub mod eventlog;
pub mod extensions;
pub mod freeze;
mod gpu;
mod http;
mod iolimit;
pub mod memory;
pub mod nt;
pub mod pipe;
pub mod power;
pub mod process;
pub mod service;
pub mod services;
pub mod settings;
pub mod startup;
pub mod storage;
pub mod user;
pub mod wake;
pub mod window;

pub use apps::installed_applications;
pub use boot::{boot_culprits, boot_history, BootCulprit, BootRecord};
pub use budget::{apply_self_limits, own_memory, OwnMemory};
pub use clock::{monotonic_ms, now};
pub use cmdline::{browser_role, command_line, BrowserRole};
pub use cpu::CpuTimesBuffer;
pub use digest::sha256_hex;
pub use eventlog::daily_error_count;
pub use extensions::{installed as installed_extensions, Extension};
pub use gpu::{load_by_process as gpu_load_by_process, GpuCounter, GpuLoad};
pub use http::{fetch, fetch_text};
pub use iolimit::{IoLimit, LimitedProcess};
pub use memory::{memory_stat, system_counts, SystemCounts};
pub use power::{power_status, PowerSource, PowerStatus};
pub use process::{
    has_hung_window, hung_process_ids, terminate as terminate_process, ProcessBuffer, ProcessIter,
    RawProcess,
};
pub use service::{
    install as install_service, service_start, set_service_start, uninstall as uninstall_service,
    ServiceStart, StopSignal,
};
pub use services::{service_by_pid, service_names, stop_service, ServiceOwner};
pub use settings::{
    autopilot_enabled, set_autopilot_enabled, set_show_widget_on_start, show_widget_on_start,
};
pub use startup::{
    add_to_startup, is_in_startup, remove_from_startup, set_startup_enabled, user_startup_items,
    StartupItem,
};
pub use storage::{enumerate as enumerate_drives, read_smart, Drive};
pub use user::{double_click_time_ms, idle_ms, notification_state, NotificationState};
pub use wake::{wake_history, WakeEvent, WakeSource};
pub use window::window_titles;
